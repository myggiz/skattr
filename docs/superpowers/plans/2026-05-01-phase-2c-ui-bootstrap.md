# Phase 2.C UI bootstrap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a read-only Skattr conversation UI as a new `crates/ui/` Tauri 2 + SvelteKit crate that boots an in-process `Daemon::run`, walks first-run users through identity creation + Tor bootstrap, and renders contacts + one open conversation with live-append on `Event::MessageReceived`.

**Architecture:** Two-phase Tauri command surface — pre-daemon (`vault_exists` / `identity_init` / `vault_unlock`) for the wizard, post-daemon (`ipc_request` / `ipc_subscribe`) talking through `IpcClient` over the daemon's existing Unix socket. Wire-format additions are append-only: `Command::DaemonInfo`, `ContactSummary` projections, `Subscribe` ack `TorStatusChanged` replay, no migrations. SvelteKit consumes a transport-agnostic `IpcClient` interface so future shells swap transports without rewriting components.

**Tech Stack:** Rust 2021 stable, Tauri 2.x, SvelteKit (Svelte 5), TypeScript, ts-rs, pnpm, Vitest, Playwright, `svelte-virtual-list`, `zxcvbn-ts`. Bundled Inter font (OFL 1.1). No remote CDNs, fonts, images, or analytics.

**Spec:** `docs/superpowers/specs/2026-05-01-phase-2c-ui-bootstrap-design.md` (locked at commit `8d536aa`).

---

## Working branch

All work runs in a dedicated worktree off `master` named `phase-2c-ui-bootstrap`. Use `superpowers:using-git-worktrees` to create it before Task 1; do not work on `master` directly.

```bash
git worktree add -b phase-2c-ui-bootstrap ../skattr-phase-2c master
cd ../skattr-phase-2c
```

---

## File map

**Modified in `crates/core/`:**
- `crates/core/src/storage/pool.rs` — add `Pool::schema_version()` accessor.
- `crates/core/src/storage/messages.rs` — add `MessageRepo::latest_for_group()`.
- `crates/core/src/daemon/commands.rs` — add `Command::DaemonInfo`; add `CommandResult::DaemonInfo`; extend `ContactSummary` with three `#[serde(default)]` fields.
- `crates/core/src/daemon/handle.rs` — add `latest_tor_status: Arc<RwLock<Option<TorStatus>>>` field + `latest_tor_status()` getter; add `set_tor_status()` setter.
- `crates/core/src/daemon/state.rs` — spawn TorStatus tap task; cache `schema_version` on the handle (extend `DaemonHandle::new_with_mailbox` if needed).
- `crates/core/src/daemon/dispatch.rs` — handle `Command::DaemonInfo`; rewrite `list_contacts` to populate new `ContactSummary` fields + apply ordering.
- `crates/core/src/daemon/ipc/server.rs` — replay cached `TorStatusChanged` after `Ok(Subscribed)` per filter.

**Created in `crates/ui/`** (new GPLv3 crate):
- `crates/ui/Cargo.toml`, `crates/ui/build.rs`, `crates/ui/tauri.conf.json`, `crates/ui/icons/` (placeholder).
- `crates/ui/src/main.rs`, `crates/ui/src/bootstrap.rs`, `crates/ui/src/daemon.rs`, `crates/ui/src/ipc_bridge.rs`, `crates/ui/src/events.rs`.
- `crates/ui/src-svelte/package.json`, `pnpm-lock.yaml`, `svelte.config.js`, `vite.config.ts`, `tsconfig.json`, `vitest.config.ts`, `playwright.config.ts`, `.gitignore`.
- `crates/ui/src-svelte/src/app.html`, `crates/ui/src-svelte/src/app.d.ts`.
- `crates/ui/src-svelte/src/lib/ipc/{client.ts, tauri.ts}` (`types.ts` is generated and gitignored).
- `crates/ui/src-svelte/src/lib/stores/{tor_status.ts, contacts.ts, conversation.ts, daemon_info.ts}`.
- `crates/ui/src-svelte/src/lib/components/{ContactRow.svelte, MessageBubble.svelte, TorPill.svelte, VirtualMessageList.svelte}`.
- `crates/ui/src-svelte/src/lib/tokens.css`, `crates/ui/src-svelte/src/lib/fonts/{inter-regular.woff2, inter-medium.woff2, OFL.txt}`.
- `crates/ui/src-svelte/src/routes/{+layout.svelte, +page.svelte, first-run/+page.svelte, first-run/{Welcome,Passphrase,SeedPhrase,Bootstrap}.svelte}`.
- `crates/ui/src-svelte/tests/` — Playwright + Vitest specs.

**Created in `crates/tests/`:**
- `crates/tests/src/ui_first_run.rs` — `#[ignore]`-gated full first-run integration test.

**Modified at workspace root:**
- `Cargo.toml` — add `crates/ui` to `members`; add `ts-rs` and `parking_lot` to workspace deps.
- `CHANGELOG.md` — Phase 2.C entry.
- `CLAUDE.md` — Repository state update.

---

## Task ordering rationale

Phase A: core wire-surface additions land first (no UI churn) so Phase 2.D inherits them cleanly even if 2.C's UI lane stalls. Phase B introduces `ts-rs` derives so codegen has something to emit. Phase C builds the Rust shell of `crates/ui/`. Phase D scaffolds SvelteKit. Phase E wires the wizard. Phase F wires the main shell. Phase G adds tests. Phase H is verification + docs.

Each task ends with a commit. The plan target is ~50 commits — match the granularity of Phases 1.G and 2.B.

---

## Phase A — Core wire-surface additions

### Task 1: `Pool::schema_version()` accessor

**Files:**
- Modify: `crates/core/src/storage/pool.rs`

- [ ] **Step 1: Write the failing test**

Append at end of `crates/core/src/storage/pool.rs` (inside the existing `#[cfg(test)] mod tests`, or in a new test module if there isn't one):

```rust
#[cfg(test)]
mod schema_version_tests {
    use super::*;
    use zeroize::Zeroizing;

    #[test]
    fn schema_version_returns_latest_after_open() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = Zeroizing::new([0u8; 32]);
        let pool = Pool::open(tmp.path(), &seed).unwrap();
        // The migration count must be > 0 for this test to be meaningful.
        let v = pool.schema_version().unwrap();
        assert!(v >= 9, "expected schema_version >= 9, got {v}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --features test-harness schema_version_returns_latest_after_open
```

Expected: FAIL — `schema_version` is not a method on `Pool`.

- [ ] **Step 3: Implement `Pool::schema_version`**

Add to `impl Pool` block in `crates/core/src/storage/pool.rs`:

```rust
/// Return the highest applied migration version. Reads from the
/// `schema_version` table that the migrations runner maintains.
pub fn schema_version(&self) -> Result<u32> {
    self.with(|c| {
        let v: u32 = c
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| {
                crate::error::CoreError::Storage(
                    crate::error::StorageErrorKind::Other(format!(
                        "schema_version: {e}"
                    )),
                )
            })?;
        Ok(v)
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p skattr-core --features test-harness schema_version_returns_latest_after_open
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/pool.rs
git commit -m "storage: Pool::schema_version() accessor for DaemonInfo"
```

---

### Task 2: `MessageRepo::latest_for_group()`

**Files:**
- Modify: `crates/core/src/storage/messages.rs`

- [ ] **Step 1: Write the failing test**

Append at end of `crates/core/src/storage/messages.rs` inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn latest_for_group_returns_max_id_row() {
    use crate::storage::Pool;
    use zeroize::Zeroizing;

    let tmp = tempfile::tempdir().unwrap();
    let seed = Zeroizing::new([1u8; 32]);
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let repo = MessageRepo::new(&pool);

    let group_id = vec![0xAA; 32];
    // Insert two rows; latest_for_group should return the higher-id one.
    let env_a = sample_envelope(*b"A___________aaaa", "first");
    let env_b = sample_envelope(*b"B___________bbbb", "second");
    repo.insert(&env_a, &group_id, &[1; 32], 0, 100).unwrap();
    repo.insert(&env_b, &group_id, &[1; 32], 0, 200).unwrap();

    let out = repo.latest_for_group(&group_id).unwrap();
    let row = out.expect("at least one row");
    assert_eq!(row.ts_daemon_recv, 200);
}

#[test]
fn latest_for_group_returns_none_when_empty() {
    use crate::storage::Pool;
    use zeroize::Zeroizing;
    let tmp = tempfile::tempdir().unwrap();
    let seed = Zeroizing::new([2u8; 32]);
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let repo = MessageRepo::new(&pool);
    assert!(repo.latest_for_group(&[0xBB; 32]).unwrap().is_none());
}
```

If `sample_envelope` doesn't already exist as a test helper, add it inside the same test module:

```rust
#[cfg(test)]
fn sample_envelope(id: [u8; 16], body: &str) -> crate::envelope::Envelope {
    crate::envelope::Envelope {
        v: 1,
        id: crate::envelope::MessageId(id),
        ts: 1_700_000_000,
        reply_to: None,
        kind: crate::envelope::Kind::Text { body: body.into() },
    }
}
```

(Re-use the existing helper if one is already present — search the file for `sample_envelope` first.)

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p skattr-core --features test-harness latest_for_group
```

Expected: FAIL — method does not exist.

- [ ] **Step 3: Implement `latest_for_group`**

Add to `impl MessageRepo` block in `crates/core/src/storage/messages.rs`:

```rust
/// Return the most recently inserted message in `group_id`, or
/// `None` if the group is empty. Used by `dispatch::list_contacts`
/// to populate `ContactSummary::last_message_preview` and
/// `last_ts_recv`.
///
/// SQL plan: the existing `(group_id, id)` index from migration 0001
/// makes this an index-scan with `LIMIT 1` — constant cost regardless
/// of group size.
pub fn latest_for_group(
    &self,
    group_id: &[u8],
) -> Result<Option<StoredMessage>> {
    self.pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                        mls_generation, ts_daemon_recv \
                 FROM messages \
                 WHERE group_id = ?1 \
                 ORDER BY id DESC \
                 LIMIT 1",
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "prepare latest_for_group: {e}"
                )))
            })?;
        let mut rows = stmt
            .query_map(rusqlite::params![group_id], |r| {
                Ok(StoredMessage {
                    id: r.get(0)?,
                    group_id: r.get(1)?,
                    sender: r.get(2)?,
                    kind: r.get(3)?,
                    body_blob: r.get(4)?,
                    ts: r.get(5)?,
                    delivered_at: r.get(6)?,
                    mls_generation: r.get(7)?,
                    ts_daemon_recv: r.get(8)?,
                })
            })
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "query latest_for_group: {e}"
                )))
            })?;
        match rows.next() {
            None => Ok(None),
            Some(Ok(r)) => Ok(Some(r)),
            Some(Err(e)) => Err(CoreError::Storage(StorageErrorKind::Other(
                format!("collect latest_for_group: {e}"),
            ))),
        }
    })
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --features test-harness latest_for_group
```

Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/messages.rs
git commit -m "storage: MessageRepo::latest_for_group for ContactSummary projections"
```

---

### Task 3: `Command::DaemonInfo` + `CommandResult::DaemonInfo` variants

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/daemon/commands.rs`:

```rust
#[test]
fn daemon_info_command_round_trips_cbor() {
    let cmd = Command::DaemonInfo;
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&cmd, &mut buf).unwrap();
    let back: Command = ciborium::de::from_reader(&buf[..]).unwrap();
    assert!(matches!(back, Command::DaemonInfo));
}

#[test]
fn daemon_info_result_round_trips_cbor() {
    let r = CommandResult::DaemonInfo {
        local_pubkey: PublicKey([0xAB; 32]),
        current_onion: Some("abcd.onion".into()),
        daemon_version: "0.0.1".into(),
        schema_version: 9,
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&r, &mut buf).unwrap();
    let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
    match back {
        CommandResult::DaemonInfo {
            current_onion,
            schema_version,
            ..
        } => {
            assert_eq!(current_onion.as_deref(), Some("abcd.onion"));
            assert_eq!(schema_version, 9);
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }
}

#[test]
fn daemon_info_result_with_none_onion_round_trips() {
    let r = CommandResult::DaemonInfo {
        local_pubkey: PublicKey([0xCD; 32]),
        current_onion: None,
        daemon_version: "0.0.1".into(),
        schema_version: 9,
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&r, &mut buf).unwrap();
    let back: CommandResult = ciborium::de::from_reader(&buf[..]).unwrap();
    match back {
        CommandResult::DaemonInfo { current_onion, .. } => {
            assert!(current_onion.is_none());
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p skattr-core --features test-harness daemon_info
```

Expected: FAIL — variants do not exist.

- [ ] **Step 3: Add `Command::DaemonInfo`**

In `crates/core/src/daemon/commands.rs`, add a variant inside the `pub enum Command { … }` (anywhere — list keeps current order, append after `ListMailboxes`):

```rust
    /// Return runtime metadata for the UI's About screen + first-paint
    /// store hydration: identity pubkey, current onion (None until
    /// Tor bootstraps), daemon version, schema version.
    DaemonInfo,
```

- [ ] **Step 4: Add `CommandResult::DaemonInfo`**

In the same file, append a variant inside `pub enum CommandResult { … }`:

```rust
    /// [`Command::DaemonInfo`] completed.
    DaemonInfo {
        /// Local Ed25519 identity pubkey.
        local_pubkey: PublicKey,
        /// Current v3 onion address (without `:port`). `None` while
        /// Tor is still bootstrapping.
        current_onion: Option<String>,
        /// `env!("CARGO_PKG_VERSION")` of `skattr-core`.
        daemon_version: String,
        /// Latest applied storage migration version.
        schema_version: u32,
    },
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon_info
```

Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/commands.rs
git commit -m "commands: Command::DaemonInfo + CommandResult::DaemonInfo variants"
```

---

### Task 4: Extend `ContactSummary` with new projection fields

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing test module in `crates/core/src/daemon/commands.rs`:

```rust
#[test]
fn contact_summary_with_new_fields_round_trips() {
    let s = ContactSummary {
        pubkey: PublicKey([0x99; 32]),
        nickname: None,
        onion: "x.onion".into(),
        card_version: 1,
        added_at: 1_700_000_000,
        unread_count: 3,
        last_message_preview: Some("hello".into()),
        last_ts_recv: Some(1_700_000_500),
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&s, &mut buf).unwrap();
    let back: ContactSummary = ciborium::de::from_reader(&buf[..]).unwrap();
    assert_eq!(back.unread_count, 3);
    assert_eq!(back.last_message_preview.as_deref(), Some("hello"));
    assert_eq!(back.last_ts_recv, Some(1_700_000_500));
}

#[test]
fn contact_summary_decodes_old_payload_with_defaults() {
    // Encode an old-shape ContactSummary using a temporary local struct
    // matching the pre-2.C schema, then decode as the new ContactSummary.
    // The new fields must default cleanly via #[serde(default)].
    #[derive(serde::Serialize)]
    struct OldShape {
        pubkey: PublicKey,
        nickname: Option<String>,
        onion: String,
        card_version: u64,
        added_at: u64,
    }
    let old = OldShape {
        pubkey: PublicKey([0x22; 32]),
        nickname: Some("legacy".into()),
        onion: "y.onion".into(),
        card_version: 7,
        added_at: 1_700_000_000,
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&old, &mut buf).unwrap();
    let back: ContactSummary = ciborium::de::from_reader(&buf[..]).unwrap();
    assert_eq!(back.unread_count, 0);
    assert!(back.last_message_preview.is_none());
    assert!(back.last_ts_recv.is_none());
    assert_eq!(back.nickname.as_deref(), Some("legacy"));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p skattr-core --features test-harness contact_summary_with_new_fields
```

Expected: FAIL — new fields don't exist.

- [ ] **Step 3: Extend `ContactSummary`**

In `crates/core/src/daemon/commands.rs`, append to the `ContactSummary` struct:

```rust
pub struct ContactSummary {
    /// Ed25519 identity pubkey.
    pub pubkey: PublicKey,
    /// User-settable local nickname.
    pub nickname: Option<String>,
    /// Onion address from the latest verified `ContactCard`.
    pub onion: String,
    /// Version of the latest known `ContactCard`.
    pub card_version: u64,
    /// Unix seconds when the contact was first added locally.
    pub added_at: u64,
    /// Number of unread messages in this contact's group, counted
    /// against the per-group `read_state` cursor. `0` for fresh
    /// contacts.
    #[serde(default)]
    pub unread_count: u64,
    /// First ≤80 Unicode code points of the latest message body.
    /// `None` when the latest message is not `Kind::Text`, or when
    /// the contact has no messages.
    #[serde(default)]
    pub last_message_preview: Option<String>,
    /// `MAX(ts_daemon_recv)` across both directions in this
    /// contact's group; `None` if zero messages.
    #[serde(default)]
    pub last_ts_recv: Option<u64>,
}
```

Update each in-file constructor / test that builds a `ContactSummary` literal to include the new fields (search the file for `ContactSummary {` — there are two such uses inside the existing test module). Both uses should set `unread_count: 0, last_message_preview: None, last_ts_recv: None`.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --features test-harness contact_summary
```

Expected: PASS (existing tests + the two new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/commands.rs
git commit -m "commands: ContactSummary projections (unread/preview/last_ts_recv)"
```

---

### Task 5: Cache the latest `TorStatus` on `DaemonHandle`

**Files:**
- Modify: `crates/core/src/daemon/handle.rs`

- [ ] **Step 1: Write the failing test**

Append at the end of `crates/core/src/daemon/handle.rs`:

```rust
#[cfg(test)]
mod tor_status_cache_tests {
    use super::*;
    use crate::daemon::events::TorStatus;
    use std::sync::Arc;

    fn fake_handle() -> DaemonHandle<tokio::io::DuplexStream> {
        let (events_tx, _) = tokio::sync::broadcast::channel(16);
        let pool = Arc::new(crate::storage::Pool::open_in_memory_for_test().unwrap());
        let identity = crate::identity::IdentityKey::generate().unwrap();
        let hub = Arc::new(crate::delivery::hub::DeliveryHub::new_no_inbound(
            pool.clone(),
        ));
        DaemonHandle::new(pool, hub, identity, events_tx)
    }

    #[test]
    fn latest_tor_status_starts_none() {
        let h = fake_handle();
        assert!(h.latest_tor_status().is_none());
    }

    #[test]
    fn set_tor_status_round_trips() {
        let h = fake_handle();
        h.set_tor_status(TorStatus::Bootstrapping(42));
        assert_eq!(
            h.latest_tor_status(),
            Some(TorStatus::Bootstrapping(42)),
        );
        h.set_tor_status(TorStatus::Ready);
        assert_eq!(h.latest_tor_status(), Some(TorStatus::Ready));
    }
}
```

If `Pool::open_in_memory_for_test` or `DeliveryHub::new_no_inbound` aren't already available behind `#[cfg(feature = "test-harness")]`, search `crates/core/src/test_exports.rs` for the equivalent test helpers and substitute the names that exist. The shape of the test is what matters — adapt accessor names to match.

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p skattr-core --features test-harness latest_tor_status
```

Expected: FAIL — methods don't exist.

- [ ] **Step 3: Add the cache field + accessors**

In `crates/core/src/daemon/handle.rs`:

Add at the top of the file (with the other use statements):

```rust
use crate::daemon::events::TorStatus;
```

Add a field to `DaemonHandle<S>`:

```rust
    /// Snapshot of the latest `TorStatusChanged` event the daemon
    /// emitted. Updated by a tap task spawned in `Daemon::run`. Read
    /// by the IPC server when answering a `Subscribe` ack so the UI
    /// can paint the bootstrap pill on first connect without waiting
    /// for the next live event.
    pub(crate) latest_tor_status: Arc<RwLock<Option<TorStatus>>>,
```

In the existing `DaemonHandle::new` constructor, add the field initializer:

```rust
            latest_tor_status: Arc::new(RwLock::new(None)),
```

Same for `DaemonHandle::new_with_mailbox` (if it exists in this file).

Add accessors to the `impl<S> DaemonHandle<S>` block:

```rust
/// Snapshot the latest cached `TorStatus`. Non-blocking RwLock read.
#[must_use]
pub fn latest_tor_status(&self) -> Option<TorStatus> {
    self.latest_tor_status
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// Replace the cached `TorStatus`. Called by the tap task spawned
/// in `Daemon::run`. Tests may call directly.
pub fn set_tor_status(&self, status: TorStatus) {
    if let Ok(mut guard) = self.latest_tor_status.write() {
        *guard = Some(status);
    }
}
```

If the existing `clone_for_dispatch` (or equivalent) method on `DaemonHandle` builds a sibling Arc-of-fields struct, propagate `latest_tor_status: self.latest_tor_status.clone()` there too — the dispatcher needs to read the cache.

- [ ] **Step 4: Run test**

```bash
cargo test -p skattr-core --features test-harness latest_tor_status
```

Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/handle.rs
git commit -m "handle: latest_tor_status cache for Subscribe ack replay"
```

---

### Task 6: Spawn the TorStatus tap task in `Daemon::run`

**Files:**
- Modify: `crates/core/src/daemon/state.rs`

- [ ] **Step 1: Identify the insertion point**

Locate the section in `Daemon::run` after `DaemonHandle::<...>::new_with_mailbox(...)` is constructed (around `// Step 6: DaemonHandle.` in `state.rs:188`). The tap task must be spawned **after** `DaemonHandle` exists (so the tap can clone its `latest_tor_status` Arc) and **before** `shutdown.await;`. It must terminate on the same shutdown signal.

- [ ] **Step 2: Add the tap task**

Right after `handle.set_onion(onion.clone());` (and before `// Step 7: IPC server.`), add:

```rust
    // TorStatus tap: subscribe to the broadcast channel and copy
    // every TorStatusChanged into the same Arc<RwLock<…>> the
    // IpcServer reads via DaemonHandle::latest_tor_status(). Spawned
    // after DaemonHandle is built so the tap and the readers share
    // the same allocation. Held on a JoinHandle so the shutdown
    // path can abort it.
    let tor_status_cache_for_tap = handle.latest_tor_status.clone();
    let mut tap_rx = events_tx.subscribe();
    let tor_tap_task = tokio::spawn(async move {
        loop {
            match tap_rx.recv().await {
                Ok(crate::daemon::events::Event::TorStatusChanged(s)) => {
                    if let Ok(mut g) = tor_status_cache_for_tap.write() {
                        *g = Some(s);
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Receiver self-recovers; loop will resume.
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
```

The field access `handle.latest_tor_status` is `pub(crate)` (declared in Task 5) — `state.rs` is in the same crate, so it compiles. No import additions are required for this snippet beyond what `state.rs` already has.

In the shutdown teardown section, abort + join the tap task:

```rust
    tor_tap_task.abort();
    let _ = tor_tap_task.await;
```

Place this right after the existing `let _ = ipc_task.await;` line.

- [ ] **Step 3: Compile**

```bash
cargo build -p skattr-core
```

Expected: clean build.

- [ ] **Step 4: Run the existing run-readiness test (optional, gated)**

```bash
cargo test -p skattr-core --features test-harness run_signals_ready -- --ignored
```

This bootstraps real Tor — only run if the network is up. Otherwise rely on the next task's IPC server replay test for end-to-end coverage.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/state.rs crates/core/src/daemon/handle.rs
git commit -m "daemon: TorStatus tap task feeding latest_tor_status cache"
```

---

### Task 7: IPC server — replay cached `TorStatus` after `Subscribe` ack

**Files:**
- Modify: `crates/core/src/daemon/ipc/server.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/daemon/ipc/server.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_ack_replays_cached_tor_status() {
    use crate::daemon::events::{Event, TorStatus};
    use crate::daemon::ipc::wire::{EventFilter, IpcRequest, IpcResponse};
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("ipc.sock");

    // Build a fake executor that returns Ok(Subscribed) for any command
    // (we only exercise the Subscribe path here).
    let (events_tx, _) = tokio::sync::broadcast::channel::<Event>(16);
    let exec_events_tx = events_tx.clone();
    // Cache one TorStatus so the server replays it.
    events_tx
        .send(Event::TorStatusChanged(TorStatus::Bootstrapping(7)))
        .ok();
    // Wait one task tick to let the tap task install the snapshot. In
    // this unit test we drive replay directly via a fake executor that
    // exposes the cache. See the module-level NoopExecutor harness.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let executor: Arc<dyn CommandExecutor> = Arc::new(NoopExecutor::with_tor_status(
        TorStatus::Bootstrapping(7),
    ));
    let server = Server::bind(&sock, current_uid()).unwrap();
    let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();
    let exec = executor.clone();
    let evs = exec_events_tx.clone();
    let task = tokio::spawn(async move {
        serve(server, exec, evs, async {
            let _ = sd_rx.await;
        })
        .await;
    });

    // Connect + Subscribe(All); expect Ok(Subscribed) then one Event(TorStatusChanged).
    let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
    write_frame(&mut client, &IpcRequest::Subscribe(EventFilter::All))
        .await
        .unwrap();

    let r1 = read_frame::<IpcResponse>(&mut client).await.unwrap();
    assert!(matches!(
        r1,
        IpcResponse::Ok(crate::daemon::commands::CommandResult::Subscribed)
    ));

    let r2 = read_frame::<IpcResponse>(&mut client).await.unwrap();
    assert!(matches!(
        r2,
        IpcResponse::Event(Event::TorStatusChanged(TorStatus::Bootstrapping(7)))
    ));

    let _ = sd_tx.send(());
    let _ = task.await;
}
```

You may need to introduce a `NoopExecutor::with_tor_status(...)` test helper inside the same `#[cfg(test)] mod tests` that yields a `latest_tor_status()` matching the argument. Pattern:

```rust
struct NoopExecutor {
    tor_status: parking_lot::RwLock<Option<crate::daemon::events::TorStatus>>,
}

impl NoopExecutor {
    fn with_tor_status(s: crate::daemon::events::TorStatus) -> Self {
        Self {
            tor_status: parking_lot::RwLock::new(Some(s)),
        }
    }
}

#[async_trait::async_trait]
impl CommandExecutor for NoopExecutor {
    async fn execute(
        &self,
        _: crate::daemon::commands::Command,
    ) -> std::result::Result<
        crate::daemon::commands::CommandResult,
        crate::daemon::ipc::wire::IpcError,
    > {
        Ok(crate::daemon::commands::CommandResult::Ok)
    }
    fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
        self.tor_status.read().clone()
    }
}
```

If `parking_lot` isn't already a dep of `skattr-core`, use `std::sync::RwLock` instead — no functional difference here.

- [ ] **Step 2: Extend `CommandExecutor` trait**

In `crates/core/src/daemon/ipc/server.rs`, locate the `pub trait CommandExecutor` definition and add a new method (with a default impl returning `None` so existing impls compile):

```rust
/// Snapshot the latest cached `TorStatus`. Default `None` so test
/// stubs and pre-2.C executors compile unchanged. The production
/// `DaemonHandle` impl returns `latest_tor_status()`.
fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
    None
}
```

Override the default in the `impl CommandExecutor for DaemonHandle<S>` block (in `crates/core/src/daemon/handle.rs` — search for `impl<S> CommandExecutor for ...`):

```rust
fn latest_tor_status(&self) -> Option<crate::daemon::events::TorStatus> {
    DaemonHandle::latest_tor_status(self)
}
```

- [ ] **Step 3: Wire the replay in `handle_connection`**

In `crates/core/src/daemon/ipc/server.rs`, replace the existing `IpcRequest::Subscribe` arm:

```rust
            IpcRequest::Subscribe(filter) => {
                subscribed = Some(filter.clone());
                events_rx = Some(events_tx.subscribe());
                if write_frame(&mut stream, &IpcResponse::Ok(CommandResult::Subscribed))
                    .await
                    .is_err()
                {
                    break;
                }
                // 2.C: replay cached TorStatus immediately if the filter
                // matches. The cache is populated by the tap task in
                // Daemon::run; lag-induced gaps fall through (None).
                if let Some(status) = executor.latest_tor_status() {
                    let replay = Event::TorStatusChanged(status);
                    if event_matches(&replay, subscribed.as_ref()) {
                        if write_frame(&mut stream, &IpcResponse::Event(replay))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p skattr-core --features test-harness subscribe_ack_replays_cached_tor_status
```

Expected: PASS.

- [ ] **Step 5: Run all IPC tests for regression**

```bash
cargo test -p skattr-core --features test-harness ipc::
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/ipc/server.rs crates/core/src/daemon/handle.rs
git commit -m "ipc: replay cached TorStatus after Subscribe ack (filter-gated)"
```

---

### Task 8: Dispatch `Command::DaemonInfo`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing dispatch test module (`#[cfg(test)] mod tests` near the bottom of `crates/core/src/daemon/dispatch.rs`):

```rust
#[tokio::test]
async fn daemon_info_returns_pubkey_onion_version_schema() {
    let h = test_handle().await;
    h.set_onion("example.onion".to_string());
    let result = execute_command(h.clone(), Command::DaemonInfo).await;
    match result.unwrap() {
        CommandResult::DaemonInfo {
            local_pubkey,
            current_onion,
            daemon_version,
            schema_version,
        } => {
            assert_eq!(local_pubkey, h.identity.public_key());
            assert_eq!(current_onion.as_deref(), Some("example.onion"));
            assert_eq!(daemon_version, env!("CARGO_PKG_VERSION"));
            assert!(schema_version >= 9);
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn daemon_info_returns_none_onion_when_not_yet_published() {
    let h = test_handle().await;
    // Do not call set_onion.
    let result = execute_command(h, Command::DaemonInfo).await;
    match result.unwrap() {
        CommandResult::DaemonInfo { current_onion, .. } => {
            assert!(current_onion.is_none());
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }
}
```

`test_handle` is the existing helper in this file. If its signature differs slightly (e.g., already returns `Arc<DaemonHandle<…>>`), match the existing pattern. `IdentityKey::public_key()` may be called `pubkey()` — adapt if needed.

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p skattr-core --features test-harness daemon_info_returns
```

Expected: FAIL — the dispatcher hasn't routed `DaemonInfo`.

- [ ] **Step 3: Add the handler**

In `crates/core/src/daemon/dispatch.rs`, locate the `match cmd { … }` in `execute_command`. Add a new arm before the closing brace:

```rust
        Command::DaemonInfo => handle_daemon_info(&handle).await,
```

Then add the handler function (after `list_contacts` is fine):

```rust
async fn handle_daemon_info<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let local_pubkey = handle.identity.public_key();
    let current_onion = handle.onion();
    let daemon_version = env!("CARGO_PKG_VERSION").to_string();
    let schema_version = handle.pool.schema_version().map_err(map_err)?;
    Ok(CommandResult::DaemonInfo {
        local_pubkey,
        current_onion,
        daemon_version,
        schema_version,
    })
}
```

If `IdentityKey` exposes the pubkey via a different name (`public()`, `pubkey()`, etc.), adapt accordingly — search the file for existing uses.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon_info_returns
```

Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "dispatch: handle Command::DaemonInfo"
```

---

### Task 9: Rewrite `dispatch::list_contacts` to populate new `ContactSummary` fields + apply ordering

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

Append to the dispatch test module:

```rust
#[tokio::test]
async fn list_contacts_populates_new_projection_fields() {
    use crate::envelope::{Envelope, Kind, MessageId};

    let h = test_handle().await;
    let repo = crate::storage::ContactRepo::new(&h.pool);
    // Insert two contacts: one with a message in its group, one without.
    let pk_a = crate::identity::PublicKey([0xAA; 32]);
    let pk_b = crate::identity::PublicKey([0xBB; 32]);
    let group_a = vec![1u8; 32];
    let group_b = vec![2u8; 32];
    repo.upsert(&crate::contact::Contact {
        identity: pk_a,
        display_name: Some("alice".into()),
        added_at: 100,
        card: None,
    })
    .unwrap();
    repo.upsert(&crate::contact::Contact {
        identity: pk_b,
        display_name: Some("bob".into()),
        added_at: 200,
        card: None,
    })
    .unwrap();
    repo.set_group_id(&pk_a, &group_a).unwrap();
    repo.set_group_id(&pk_b, &group_b).unwrap();

    // alice has a recent message; bob has none.
    let env = Envelope {
        v: 1,
        id: MessageId([0x01; 16]),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text {
            body: "yo this is a preview".into(),
        },
    };
    crate::storage::MessageRepo::new(&h.pool)
        .insert(&env, &group_a, &pk_a.0, 0, 1_700_000_500)
        .unwrap();

    let result = execute_command(h, Command::ListContacts).await.unwrap();
    let summaries = match result {
        CommandResult::Contacts(v) => v,
        other => panic!("expected Contacts, got {other:?}"),
    };
    // Order: alice first (last_ts_recv newer); bob last (None → NULLS LAST).
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].pubkey, pk_a);
    assert_eq!(
        summaries[0].last_message_preview.as_deref(),
        Some("yo this is a preview"),
    );
    assert_eq!(summaries[0].last_ts_recv, Some(1_700_000_500));
    assert_eq!(summaries[1].pubkey, pk_b);
    assert!(summaries[1].last_message_preview.is_none());
    assert!(summaries[1].last_ts_recv.is_none());
}

#[tokio::test]
async fn list_contacts_truncates_preview_to_80_codepoints() {
    use crate::envelope::{Envelope, Kind, MessageId};

    let h = test_handle().await;
    let pk = crate::identity::PublicKey([0xCC; 32]);
    let group = vec![3u8; 32];
    crate::storage::ContactRepo::new(&h.pool)
        .upsert(&crate::contact::Contact {
            identity: pk,
            display_name: None,
            added_at: 0,
            card: None,
        })
        .unwrap();
    crate::storage::ContactRepo::new(&h.pool)
        .set_group_id(&pk, &group)
        .unwrap();

    let body = "x".repeat(200);
    let env = Envelope {
        v: 1,
        id: MessageId([0x02; 16]),
        ts: 0,
        reply_to: None,
        kind: Kind::Text { body: body.clone() },
    };
    crate::storage::MessageRepo::new(&h.pool)
        .insert(&env, &group, &pk.0, 0, 100)
        .unwrap();

    let r = execute_command(h, Command::ListContacts).await.unwrap();
    let summaries = match r {
        CommandResult::Contacts(v) => v,
        other => panic!("{other:?}"),
    };
    let preview = summaries[0].last_message_preview.as_ref().unwrap();
    assert_eq!(preview.chars().count(), 80);
    assert!(preview.chars().all(|c| c == 'x'));
}
```

`ContactRepo::set_group_id` and `Contact` field names should already match these uses (see `dispatch.rs`'s existing `add_contact` for the pattern).

- [ ] **Step 2: Run tests**

```bash
cargo test -p skattr-core --features test-harness list_contacts_populates list_contacts_truncates
```

Expected: FAIL (fields aren't populated yet).

- [ ] **Step 3: Replace the body of `list_contacts`**

Replace the existing `async fn list_contacts<S>` body in `crates/core/src/daemon/dispatch.rs` with:

```rust
async fn list_contacts<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::ContactSummary;
    use crate::storage::{ContactRepo, MessageRepo};

    let repo = ContactRepo::new(&handle.pool);
    let msg_repo = MessageRepo::new(&handle.pool);
    let contacts = repo.list().map_err(map_err)?;

    let mut summaries: Vec<ContactSummary> = Vec::with_capacity(contacts.len());
    for c in contacts {
        let (onion, card_version) = c
            .card
            .as_ref()
            .map(|card| (card.body.onion.clone(), card.body.version))
            .unwrap_or_else(|| (String::new(), 0));

        // Group-scoped projections. Contacts without a group_id (e.g.,
        // not yet welcomed) yield zeros / Nones.
        let group_id = repo.get_group_id(&c.identity).map_err(map_err)?;
        let (unread_count, last_message_preview, last_ts_recv) = match group_id {
            Some(gid) => {
                let unread = msg_repo.unread_count(&gid).map_err(map_err)?;
                let latest = msg_repo.latest_for_group(&gid).map_err(map_err)?;
                let preview = latest.as_ref().and_then(|row| {
                    let env: crate::envelope::Envelope =
                        ciborium::de::from_reader(&row.body_blob[..]).ok()?;
                    match env.kind {
                        crate::envelope::Kind::Text { body } => {
                            Some(truncate_preview(&body, 80))
                        }
                        _ => None,
                    }
                });
                let ts = latest.map(|row| u64::try_from(row.ts_daemon_recv).unwrap_or(0));
                (unread, preview, ts)
            }
            None => (0, None, None),
        };

        summaries.push(ContactSummary {
            pubkey: c.identity,
            nickname: c.display_name,
            onion,
            card_version,
            added_at: u64::try_from(c.added_at).unwrap_or(0),
            unread_count,
            last_message_preview,
            last_ts_recv,
        });
    }

    // Order: last_ts_recv DESC NULLS LAST, added_at DESC. Sort in
    // Rust because the underlying repo doesn't expose a JOIN.
    summaries.sort_by(|a, b| {
        match (b.last_ts_recv, a.last_ts_recv) {
            (Some(bv), Some(av)) => bv.cmp(&av).then(b.added_at.cmp(&a.added_at)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.added_at.cmp(&a.added_at),
        }
    });

    Ok(CommandResult::Contacts(summaries))
}

/// Truncate `s` to at most `max_chars` Unicode code points. Cheap
/// 2.C-grade preview; grapheme-aware truncation lands in 2.D.
fn truncate_preview(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}
```

The body_blob deserialization assumes `messages.body_blob` stores a CBOR-encoded `Envelope`. If the actual schema stores a raw `Kind` instead, search `MessageRepo::insert` for the encoding step and adapt the decode path. Check `crates/core/src/storage/messages.rs` for the canonical write side and mirror it.

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --features test-harness list_contacts
```

Expected: PASS for the two new tests + the existing `list_contacts_returns_all_rows_projected` (which should still pass — it constructs `ContactSummary` literals; verify the test was updated in Task 4 to include the new fields).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "dispatch: list_contacts populates unread/preview/last_ts_recv with sort"
```

---

## Phase B — `ts-rs` derives for wire types

### Task 10: Add `ts-rs` to workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/core/Cargo.toml`

- [ ] **Step 1: Add to workspace deps**

In `Cargo.toml` at the repo root, under `[workspace.dependencies]`, append:

```toml
ts-rs = { version = "9", default-features = false, features = ["serde-compat", "no-serde-warnings"] }
```

If `parking_lot` is not yet listed there (it isn't currently), also add:

```toml
parking_lot = "0.12"
```

- [ ] **Step 2: Add to `skattr-core` deps**

In `crates/core/Cargo.toml`, under `[dev-dependencies]` (so derive macros only run in test/codegen contexts initially) — actually, ts-rs `#[derive(TS)]` requires the dep at non-dev because we want the derive to compile for the `crates/ui` codegen path. Add to `[dependencies]`:

```toml
ts-rs.workspace = true
```

- [ ] **Step 3: Verify the workspace builds**

```bash
cargo build -p skattr-core
```

Expected: clean build (no `#[derive(TS)]` annotations yet — the dep just compiles as available).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/core/Cargo.toml
git commit -m "deps: add ts-rs (and parking_lot) to workspace deps"
```

---

### Task 11: Annotate wire types with `#[derive(TS)]`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`
- Modify: `crates/core/src/daemon/events.rs`
- Modify: `crates/core/src/daemon/ipc/wire.rs`
- Modify: `crates/core/src/daemon/error_kind.rs`
- Modify: `crates/core/src/daemon/hex.rs`
- Modify: `crates/core/src/identity/key.rs` (only `PublicKey`)
- Modify: `crates/core/src/envelope.rs` (only `Envelope`, `MessageId`, `Kind`)
- Modify: `crates/core/src/storage/mailboxes.rs` (only `MailboxStatus`)

For every wire-relevant type, add `ts_rs::TS` to the existing derive list and a `#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]` attribute. The `export` attribute makes the type emit on `cargo test`.

- [ ] **Step 1: Annotate `Command`, `CommandResult`, `ContactSummary`, `MessageRecord`, `SearchHitRecord`, `MailboxSummary`, `Direction`, `SendStatus`** in `crates/core/src/daemon/commands.rs`. Example for `Command`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/")]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command { /* ... */ }
```

Repeat the pattern for every other type listed.

- [ ] **Step 2: Annotate `Event`, `TorStatus`, `DeliveryStatus`** in `crates/core/src/daemon/events.rs`. Same `#[derive(ts_rs::TS)]` + `#[ts(export, export_to = "...")]` annotation.

- [ ] **Step 3: Annotate `EventFilter`, `IpcRequest`, `IpcResponse`, `IpcError`** in `crates/core/src/daemon/ipc/wire.rs`. Same pattern.

- [ ] **Step 4: Annotate `DaemonErrorKind`** (and the six sub-enums it contains: `StorageErrorKind`, `ContactErrorKind`, `InviteErrorKind`, `MlsErrorKind`, `DeliveryErrorKind`, `TransportErrorKind`) in `crates/core/src/daemon/error_kind.rs` and `crates/core/src/error.rs`. Same pattern.

- [ ] **Step 5: Annotate `Hex16`, `Hex32`** in `crates/core/src/daemon/hex.rs`. For newtypes, ts-rs needs `#[ts(type = "string")]` so it emits as a TS `string` rather than `{ inner: number[] }`:

```rust
#[derive(..., ts_rs::TS)]
#[ts(export, export_to = "../../../crates/ui/src-svelte/src/lib/ipc/types/", type = "string")]
pub struct Hex16(/* ... */);
```

- [ ] **Step 6: Annotate `PublicKey`** in `crates/core/src/identity/key.rs`. Same `#[ts(type = "string")]` treatment — UI sees the pubkey as a hex string.

- [ ] **Step 7: Annotate `Envelope`, `MessageId`, `Kind`** in `crates/core/src/envelope.rs`. `MessageId` gets `#[ts(type = "string")]`; `Envelope` and `Kind` use the standard pattern.

- [ ] **Step 8: Annotate `MailboxStatus`** in `crates/core/src/storage/mailboxes.rs`. Standard pattern.

- [ ] **Step 9: Run `cargo build`**

```bash
cargo build -p skattr-core
```

Expected: clean build. If any derive fails because the type contains a non-`TS` field (e.g., a `Vec<u8>`), add `#[ts(type = "string")]` or `#[ts(skip)]` on that field as appropriate.

- [ ] **Step 10: Run `cargo test`**

```bash
cargo test -p skattr-core --features test-harness
```

Expected: all PASS, plus a side effect — the export directory now contains `.ts` files. Verify:

```bash
ls crates/ui/src-svelte/src/lib/ipc/types/
```

Expected: `Command.ts`, `CommandResult.ts`, `Event.ts`, `IpcRequest.ts`, `ContactSummary.ts`, etc.

- [ ] **Step 11: Commit**

```bash
git add crates/core/src
git commit -m "ts-rs: derive TS for every wire type; emit to crates/ui types/"
```

---

### Task 12: Add aggregator `types.ts` + `.gitignore` for generated outputs

**Files:**
- Create: `crates/ui/src-svelte/src/lib/ipc/types/index.ts`
- Create: `crates/ui/src-svelte/.gitignore`

- [ ] **Step 1: Write the aggregator**

Create `crates/ui/src-svelte/src/lib/ipc/types/index.ts`:

```typescript
// Re-export every generated wire type. `cargo test -p skattr-core`
// regenerates the sibling files; do not hand-edit.
export * from "./Command";
export * from "./CommandResult";
export * from "./ContactSummary";
export * from "./MessageRecord";
export * from "./SearchHitRecord";
export * from "./MailboxSummary";
export * from "./Direction";
export * from "./SendStatus";
export * from "./Event";
export * from "./TorStatus";
export * from "./DeliveryStatus";
export * from "./EventFilter";
export * from "./IpcRequest";
export * from "./IpcResponse";
export * from "./IpcError";
export * from "./DaemonErrorKind";
export * from "./Hex16";
export * from "./Hex32";
export * from "./PublicKey";
export * from "./Envelope";
export * from "./MessageId";
export * from "./Kind";
export * from "./MailboxStatus";
```

If ts-rs naming differs for any of these (e.g., emits `IpcResponseFrame.ts` not `IpcResponse.ts`), align the export list with what's on disk.

- [ ] **Step 2: Create the SvelteKit-side `.gitignore`**

Create `crates/ui/src-svelte/.gitignore`:

```
node_modules
.svelte-kit
build
.vite
playwright-report
test-results

# ts-rs generated outputs (regenerate via `cargo test -p skattr-core`)
src/lib/ipc/types/*.ts
!src/lib/ipc/types/index.ts
```

- [ ] **Step 3: Verify the generated files are ignored**

```bash
cd crates/ui/src-svelte
git status --ignored | head -30
```

Expected: the auto-generated `Command.ts` etc. appear under "Ignored files," but `index.ts` is staged as untracked.

- [ ] **Step 4: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/.gitignore crates/ui/src-svelte/src/lib/ipc/types/index.ts
git commit -m "ui: gitignore ts-rs outputs; add types/index.ts aggregator"
```

---

## Phase C — `crates/ui/` Rust shell

### Task 13: Register `crates/ui/` in the workspace

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Update workspace members**

In `Cargo.toml` (root), update `[workspace] members`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/mailbox",
    "crates/cli",
    "crates/tests",
    "crates/ui",
]
```

- [ ] **Step 2: Don't commit yet** — the directory doesn't exist. We'll commit at the end of the next task.

---

### Task 14: Scaffold `crates/ui/Cargo.toml` + `src/main.rs`

**Files:**
- Create: `crates/ui/Cargo.toml`
- Create: `crates/ui/src/main.rs`
- Create: `crates/ui/build.rs`
- Create: `crates/ui/tauri.conf.json`
- Create: `crates/ui/icons/` (placeholder; copy from a Tauri starter or generate)

- [ ] **Step 1: Create `crates/ui/Cargo.toml`**

```toml
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Myggiz AB

[package]
name = "skattr-ui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
license = "GPL-3.0-or-later"
description = "Tauri 2 + SvelteKit desktop UI for Skattr"

[lints]
workspace = true

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
skattr-core = { path = "../core" }
tauri = { version = "2", features = [] }
tokio = { workspace = true }
serde.workspace = true
serde_json = "1"
zeroize.workspace = true
parking_lot.workspace = true
anyhow = "1"
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
tempfile = "3"

[[bin]]
name = "skattr-ui"
path = "src/main.rs"
```

If any of those workspace deps don't exist in `Cargo.toml`, add them or substitute concrete versions.

- [ ] **Step 2: Create `crates/ui/build.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

fn main() {
    tauri_build::build();
}
```

- [ ] **Step 3: Create `crates/ui/tauri.conf.json`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Skattr",
  "version": "0.0.1",
  "identifier": "net.myggiz.skattr",
  "build": {
    "frontendDist": "../src-svelte/build",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "pnpm --dir src-svelte dev",
    "beforeBuildCommand": "pnpm --dir src-svelte build"
  },
  "app": {
    "windows": [
      {
        "title": "Skattr",
        "width": 1100,
        "height": 720,
        "minWidth": 720,
        "minHeight": 480,
        "decorations": true,
        "resizable": true
      }
    ],
    "security": {
      "csp": "default-src 'self'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ipc: tauri:; script-src 'self'"
    }
  },
  "bundle": {
    "active": true,
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/icon.ico", "icons/icon.icns"],
    "category": "SocialNetworking",
    "shortDescription": "Metadata-resistant P2P encrypted messenger.",
    "longDescription": "Skattr is a desktop-first, metadata-resistant P2P encrypted messenger. All traffic over Tor v3 onion services. MLS for message encryption."
  }
}
```

- [ ] **Step 4: Create placeholder icons**

```bash
mkdir -p crates/ui/icons
# Tauri requires concrete icons before bundling. For development a
# generated 32×32 + 128×128 PNG is enough; the release pipeline will
# replace them. Use ImageMagick if available, otherwise commit empty
# placeholders that bundling can skip during dev.
convert -size 32x32 xc:'#7aa2f7' crates/ui/icons/32x32.png 2>/dev/null || \
    printf '\x89PNG\r\n\x1a\n' > crates/ui/icons/32x32.png
convert -size 128x128 xc:'#7aa2f7' crates/ui/icons/128x128.png 2>/dev/null || \
    printf '\x89PNG\r\n\x1a\n' > crates/ui/icons/128x128.png
touch crates/ui/icons/icon.ico crates/ui/icons/icon.icns
```

If your environment has neither `convert` nor a way to produce real PNGs, commit a `README.md` in `icons/` explaining icons must be generated before release. Tauri 2 will accept missing icon files in dev mode but warn.

- [ ] **Step 5: Create `crates/ui/src/main.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Skattr UI — Tauri 2 + SvelteKit shell.
//!
//! Boots the Tauri runtime with two-phase Tauri command surfaces:
//! pre-daemon (`bootstrap`) and post-daemon (`ipc_bridge` + `events`).

mod bootstrap;
mod daemon;
mod events;
mod ipc_bridge;

use tauri::Manager;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,skattr=debug")),
        )
        .init();

    tauri::Builder::default()
        .manage(daemon::AppState::default())
        .invoke_handler(tauri::generate_handler![
            bootstrap::vault_exists,
            bootstrap::identity_init,
            bootstrap::vault_unlock,
            ipc_bridge::ipc_request,
            events::ipc_subscribe,
            daemon::start_in_process_cmd,
        ])
        .setup(|app| {
            // Resolve data_dir once and stash it.
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("Tauri app data dir")
                .join("skattr");
            std::fs::create_dir_all(&data_dir).ok();
            let state: tauri::State<daemon::AppState> = app.state();
            *state.data_dir.write() = Some(data_dir);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Phase 2.C: quit-on-close. 2.F replaces with hide-to-tray.
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    daemon::shutdown(&app).await;
                    app.exit(0);
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("Tauri run");
}
```

- [ ] **Step 6: Verify the workspace skeleton compiles**

We expect failures because `bootstrap`, `daemon`, `events`, `ipc_bridge` modules don't exist yet. That's fine — the next four tasks fill them in. For now compile-fail is acceptable.

```bash
cargo check -p skattr-ui 2>&1 | head -40
```

Expected: errors about missing modules. We'll commit at the end of Task 18.

---

### Task 15: `bootstrap.rs` — `vault_exists` Tauri command

**Files:**
- Create: `crates/ui/src/bootstrap.rs`

- [ ] **Step 1: Create the file with the file-existence command**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Pre-daemon Tauri commands. These are the only Tauri commands that
//! run before `Daemon::run` is spawned. Three commands total:
//! `vault_exists`, `identity_init`, `vault_unlock`. The lint test in
//! this module enforces the cap.

use serde::{Deserialize, Serialize};

use crate::daemon::AppState;

#[tauri::command]
pub async fn vault_exists(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    Ok(data_dir.join("identity.vault").exists())
}

#[derive(Debug, Deserialize)]
pub struct IdentityInitArgs {
    pub passphrase: String,
    pub mnemonic: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IdentityInitResult {
    pub mnemonic: String,
}

#[tauri::command]
pub async fn identity_init(
    state: tauri::State<'_, AppState>,
    args: IdentityInitArgs,
) -> Result<IdentityInitResult, String> {
    use skattr_core::identity::{IdentityKey, Mnemonic, Seed, Vault};
    use zeroize::Zeroizing;

    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let vault_path = data_dir.join("identity.vault");
    if vault_path.exists() {
        return Err("vault already exists".to_string());
    }

    let seed = match args.mnemonic.as_deref() {
        Some(words) => {
            let parsed: Vec<String> =
                words.split_whitespace().map(str::to_string).collect();
            let m = Mnemonic { words: parsed };
            Seed::from_mnemonic(&m).map_err(|e| format!("bad mnemonic: {e}"))?
        }
        None => Seed::generate().map_err(|e| format!("seed gen: {e}"))?,
    };
    let mnemonic = seed.to_mnemonic().map_err(|e| format!("mnemonic: {e}"))?;
    let key = IdentityKey::from_seed(&seed).map_err(|e| format!("key: {e}"))?;

    let pass = Zeroizing::new(args.passphrase);
    Vault::create(&vault_path, key, pass.as_str())
        .map_err(|e| format!("vault create: {e}"))?;

    Ok(IdentityInitResult {
        mnemonic: mnemonic.words.join(" "),
    })
}

#[derive(Debug, Deserialize)]
pub struct VaultUnlockArgs {
    pub passphrase: String,
}

#[tauri::command]
pub async fn vault_unlock(
    state: tauri::State<'_, AppState>,
    args: VaultUnlockArgs,
) -> Result<(), String> {
    use skattr_core::identity::Vault;
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let vault_path = data_dir.join("identity.vault");
    if !vault_path.exists() {
        return Err("no vault to unlock".to_string());
    }
    let pass = zeroize::Zeroizing::new(args.passphrase.clone());
    let _ = Vault::open(&vault_path, pass.as_str())
        .map_err(|e| format!("unlock failed: {e}"))?;
    // Stash the passphrase in state for daemon::start_in_process_cmd to consume.
    *state.pending_passphrase.write() = Some(zeroize::Zeroizing::new(args.passphrase));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Lint guard: the pre-daemon Tauri command surface is restricted
    /// to three annotations. Adding a fourth requires re-evaluating
    /// the wizard-first contract from the 2.C spec.
    #[test]
    fn bootstrap_tauri_commands_are_capped_at_three() {
        let src = std::fs::read_to_string(Path::new(file!())).unwrap();
        let count = src.matches("#[tauri::command]").count();
        assert_eq!(
            count, 3,
            "bootstrap.rs must expose exactly 3 Tauri commands; got {count}"
        );
    }
}
```

If the `Mnemonic` import path differs (`skattr_core::identity::Mnemonic` may not be public — check `crates/core/src/lib.rs`'s re-exports), search the public surface and adapt. If `Mnemonic` is not yet re-exported, expose it via `crates/core/src/identity/mod.rs` with `pub use seed::Mnemonic;` in a separate one-line preparatory commit before this task.

- [ ] **Step 2: Confirm `Mnemonic` is publicly accessible**

```bash
grep -n "pub use\|pub mod" crates/core/src/identity/mod.rs
grep -n "pub struct Mnemonic" crates/core/src/identity/seed.rs
```

If `Mnemonic` is not in the public re-exports, add it to `crates/core/src/identity/mod.rs`:

```rust
pub use seed::{Mnemonic, Seed};
```

(If only `Seed` is re-exported, append `Mnemonic` next to it.)

- [ ] **Step 3: Commit (still no compilation expected — `daemon.rs` still missing)**

We'll defer commit until Task 18.

---

### Task 16: `daemon.rs` — `AppState` + `start_in_process_cmd`

**Files:**
- Create: `crates/ui/src/daemon.rs`

- [ ] **Step 1: Create the file**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! In-process `Daemon::run` lifecycle. `start_in_process_cmd` is the
//! Tauri command the wizard's final step calls after `vault_unlock`
//! has stashed the passphrase. The command spawns `Daemon::run` on a
//! Tokio task, opens an `IpcClient` against the returned socket path,
//! and parks both in `AppState` for the post-daemon command surface
//! to use.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use skattr_core::daemon::ipc::IpcClient;
use skattr_core::daemon::{Config, Daemon, Ready};

#[derive(Default)]
pub struct AppState {
    /// Resolved data directory; set in `tauri::Builder::setup`.
    pub data_dir: RwLock<Option<PathBuf>>,
    /// Passphrase captured by `bootstrap::vault_unlock` /
    /// `bootstrap::identity_init`, consumed by
    /// `start_in_process_cmd`.
    pub pending_passphrase: RwLock<Option<zeroize::Zeroizing<String>>>,
    /// Async-mutex around the post-daemon IPC client. `Some` only
    /// after `start_in_process_cmd` succeeds.
    pub ipc: Mutex<Option<IpcClient<tokio::net::UnixStream>>>,
    /// Cached `Ready` snapshot from `Daemon::run`.
    pub ready: RwLock<Option<Ready>>,
    /// Daemon task handle; held so shutdown can `abort` if needed.
    pub task: Mutex<Option<JoinHandle<skattr_core::error::Result<()>>>>,
    /// Sender for graceful daemon shutdown.
    pub shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[tauri::command]
pub async fn start_in_process_cmd(state: tauri::State<'_, AppState>) -> Result<Ready, String> {
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let passphrase = state
        .pending_passphrase
        .write()
        .take()
        .ok_or_else(|| "no pending passphrase; call vault_unlock or identity_init first".to_string())?;

    let mut config = Config::defaults().map_err(|e| format!("config: {e}"))?;
    config.data_dir = data_dir.clone();
    // Pin IPC socket to the well-known path so the CLI keeps working.
    config.ipc_socket = Some(data_dir.join("ipc.sock"));

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Ready>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let pass = passphrase.clone();
    let dd = data_dir.clone();
    let task = tokio::spawn(async move {
        Daemon::run(&dd, &pass, config, ready_tx, shutdown_fut).await
    });

    let ready = tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx)
        .await
        .map_err(|_| "Tor bootstrap timed out (180s)".to_string())?
        .map_err(|_| "ready channel closed early".to_string())?;

    let client = IpcClient::connect(&ready.ipc_socket)
        .await
        .map_err(|e| format!("ipc connect: {e}"))?;

    *state.ready.write() = Some(ready.clone());
    *state.shutdown_tx.lock().await = Some(shutdown_tx);
    *state.task.lock().await = Some(task);
    *state.ipc.lock().await = Some(client);

    Ok(ready)
}

/// Graceful shutdown — drains the daemon over the shutdown oneshot,
/// joins the task with a timeout. Called from the close-window hook.
pub async fn shutdown(app: &tauri::AppHandle) {
    let state = tauri::Manager::state::<AppState>(app);
    if let Some(tx) = state.shutdown_tx.lock().await.take() {
        let _ = tx.send(());
    }
    if let Some(handle) = state.task.lock().await.take() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
    }
}
```

If `Ready` doesn't already derive `Clone + Serialize`, add those derives in `crates/core/src/daemon/state.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct Ready { /* ... */ }
```

`PathBuf` serializes naturally if not behind a non-string path.

- [ ] **Step 2: Verify `Config::defaults`**

```bash
grep -n "fn defaults\|fn ipc_socket_or_default" crates/core/src/daemon/config.rs
```

If `Config::defaults()` doesn't exist or has a different signature, adapt the constructor accordingly (e.g., it may be `Config::new(data_dir)`).

- [ ] **Step 3: Don't commit yet** — Tasks 17 and 18 finish the shell.

---

### Task 17: `ipc_bridge.rs` — `ipc_request` Tauri command

**Files:**
- Create: `crates/ui/src/ipc_bridge.rs`

- [ ] **Step 1: Create the file**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Post-daemon Tauri command: `ipc_request`. Single generic command
//! that proxies any `IpcRequest` to the daemon over the in-process
//! `IpcClient` and returns the wire response verbatim.

use skattr_core::daemon::commands::Command;
use skattr_core::daemon::ipc::wire::IpcResponse;

use crate::daemon::AppState;

#[tauri::command]
pub async fn ipc_request(
    state: tauri::State<'_, AppState>,
    cmd: Command,
) -> Result<IpcResponse, String> {
    let mut guard = state.ipc.lock().await;
    let client = guard.as_mut().ok_or_else(|| {
        "daemon not yet running; call start_in_process_cmd first".to_string()
    })?;
    match client.execute(cmd).await {
        Ok(result) => Ok(IpcResponse::Ok(result)),
        Err(e) => Ok(IpcResponse::Err(
            skattr_core::daemon::ipc::wire::IpcError::Internal(format!("{e}")),
        )),
    }
}
```

If `IpcClient::execute` returns `Result<CommandResult, IpcClientError>`, that maps cleanly. If it returns the wire `IpcResponse` directly, simplify the body. Confirm via:

```bash
grep -n "pub async fn execute" crates/core/src/daemon/ipc/client.rs
```

- [ ] **Step 2: Don't commit yet.**

---

### Task 18: `events.rs` — `ipc_subscribe` Tauri command + relay

**Files:**
- Create: `crates/ui/src/events.rs`

- [ ] **Step 1: Create the file**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Long-lived event subscription: opens a fresh IPC connection (the
//! IpcClient in AppState handles request/response; this opens a
//! separate connection for streaming events) and relays each frame to
//! a Tauri Channel for SvelteKit to consume.

use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::ipc::IpcClient;

use crate::daemon::AppState;

#[tauri::command]
pub async fn ipc_subscribe(
    state: tauri::State<'_, AppState>,
    filter: EventFilter,
    channel: tauri::ipc::Channel<Event>,
) -> Result<(), String> {
    let socket_path = state
        .ready
        .read()
        .clone()
        .ok_or_else(|| "daemon not yet running".to_string())?
        .ipc_socket;

    // New connection per subscribe — the request/response IpcClient
    // in AppState is reserved for one-shot commands.
    let mut client = IpcClient::connect(&socket_path)
        .await
        .map_err(|e| format!("ipc connect: {e}"))?;
    client.subscribe(filter).await.map_err(|e| format!("subscribe: {e}"))?;

    tokio::spawn(async move {
        loop {
            match client.next_event().await {
                Ok(ev) => {
                    if channel.send(ev).is_err() {
                        // Receiver gone — Svelte unmounted the consumer.
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(())
}
```

Confirm `tauri::ipc::Channel<T>` is available in Tauri 2's API surface (it is — see Tauri 2 docs for `ipc::Channel`). If the import path differs in the actual Tauri 2 release used (`tauri::Channel<T>` in some versions), align.

- [ ] **Step 2: Compile the crate**

```bash
cargo check -p skattr-ui
```

Expected: clean check (warnings about missing icons in dev mode are OK).

- [ ] **Step 3: Run the lint test**

```bash
cargo test -p skattr-ui bootstrap_tauri_commands_are_capped_at_three
```

Expected: PASS.

- [ ] **Step 4: Commit (Tasks 13–18 in one foundational commit)**

```bash
git add Cargo.toml crates/ui
git commit -m "ui: scaffold crates/ui Rust shell (bootstrap + daemon + bridge + events)"
```

---

## Phase D — SvelteKit scaffold

### Task 19: SvelteKit project files (config + manifest)

**Files:**
- Create: `crates/ui/src-svelte/package.json`
- Create: `crates/ui/src-svelte/tsconfig.json`
- Create: `crates/ui/src-svelte/svelte.config.js`
- Create: `crates/ui/src-svelte/vite.config.ts`
- Create: `crates/ui/src-svelte/vitest.config.ts`
- Create: `crates/ui/src-svelte/playwright.config.ts`
- Create: `crates/ui/src-svelte/src/app.html`
- Create: `crates/ui/src-svelte/src/app.d.ts`

- [ ] **Step 1: `package.json`**

```json
{
  "name": "skattr-ui",
  "private": true,
  "version": "0.0.1",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "preview": "vite preview",
    "check": "svelte-kit sync && svelte-check --tsconfig ./tsconfig.json",
    "test": "vitest run",
    "test:watch": "vitest",
    "test:e2e": "playwright test"
  },
  "devDependencies": {
    "@playwright/test": "1.47.0",
    "@sveltejs/adapter-static": "3.0.5",
    "@sveltejs/kit": "2.5.27",
    "@sveltejs/vite-plugin-svelte": "4.0.0",
    "@testing-library/svelte": "5.2.0",
    "jsdom": "25.0.0",
    "svelte": "5.0.0",
    "svelte-check": "4.0.0",
    "tslib": "2.7.0",
    "typescript": "5.5.4",
    "vite": "5.4.0",
    "vitest": "2.1.0"
  },
  "dependencies": {
    "@tauri-apps/api": "2.0.0",
    "@zxcvbn-ts/core": "3.0.4",
    "@zxcvbn-ts/language-en": "3.0.2",
    "svelte-virtual-list": "3.0.1"
  }
}
```

If `svelte-virtual-list` 3.x doesn't support Svelte 5 (likely won't — Rich Harris's package is unmaintained), substitute `svelte-tiny-virtual-list@2` or `@tanstack/svelte-virtual@3`. Update the spec's risks-and-mitigations table afterward to record the substitution. The plan tolerates this swap because the spec's locked decision was "a virtualised list lib" with `svelte-virtual-list` as a soft pin.

- [ ] **Step 2: `tsconfig.json`**

```json
{
  "extends": "./.svelte-kit/tsconfig.json",
  "compilerOptions": {
    "allowJs": true,
    "checkJs": true,
    "esModuleInterop": true,
    "forceConsistentCasingInFileNames": true,
    "resolveJsonModule": true,
    "skipLibCheck": true,
    "sourceMap": true,
    "strict": true,
    "moduleResolution": "bundler"
  }
}
```

- [ ] **Step 3: `svelte.config.js`**

```javascript
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      pages: "build",
      assets: "build",
      fallback: "index.html",
      precompress: false,
      strict: true,
    }),
  },
};
export default config;
```

- [ ] **Step 4: `vite.config.ts`**

```typescript
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [sveltekit()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_"],
});
```

- [ ] **Step 5: `vitest.config.ts`**

```typescript
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [sveltekit()],
  test: {
    include: ["src/**/*.{test,spec}.{js,ts}"],
    environment: "jsdom",
    globals: true,
  },
});
```

- [ ] **Step 6: `playwright.config.ts`**

```typescript
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  retries: 0,
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "pnpm build && pnpm preview --port 4173",
    port: 4173,
    timeout: 120_000,
    reuseExistingServer: false,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
```

- [ ] **Step 7: `src/app.html`**

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta http-equiv="Content-Security-Policy" content="default-src 'self' tauri: ipc:; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' tauri: ipc:; script-src 'self'" />
    <link rel="icon" href="%sveltekit.assets%/favicon.png" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    %sveltekit.head%
  </head>
  <body data-sveltekit-preload-data="hover">
    <div style="display: contents">%sveltekit.body%</div>
  </body>
</html>
```

- [ ] **Step 8: `src/app.d.ts`**

```typescript
declare global {
  namespace App {}
}
export {};
```

- [ ] **Step 9: Install + check**

```bash
cd crates/ui/src-svelte
pnpm install
pnpm check
```

Expected: `pnpm install` writes `pnpm-lock.yaml`; `pnpm check` reports zero errors (SvelteKit will warn about no routes yet — ignore until routes are added in Task 28+).

- [ ] **Step 10: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/package.json crates/ui/src-svelte/pnpm-lock.yaml \
        crates/ui/src-svelte/tsconfig.json crates/ui/src-svelte/svelte.config.js \
        crates/ui/src-svelte/vite.config.ts crates/ui/src-svelte/vitest.config.ts \
        crates/ui/src-svelte/playwright.config.ts crates/ui/src-svelte/src/app.html \
        crates/ui/src-svelte/src/app.d.ts
git commit -m "ui-svelte: scaffold SvelteKit project (configs + manifest)"
```

---

### Task 20: `tokens.css` + Inter font assets

**Files:**
- Create: `crates/ui/src-svelte/src/lib/tokens.css`
- Create: `crates/ui/src-svelte/src/lib/fonts/inter-regular.woff2`
- Create: `crates/ui/src-svelte/src/lib/fonts/inter-medium.woff2`
- Create: `crates/ui/src-svelte/src/lib/fonts/OFL.txt`

- [ ] **Step 1: Write `tokens.css` with the exact locked values**

```css
/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Myggiz AB */
/* Phase 2.C design tokens — locked in 2026-05-01 spec; reused 2.D–2.F. */

@font-face {
  font-family: "Inter";
  font-style: normal;
  font-weight: 400;
  font-display: block;
  src: url("./fonts/inter-regular.woff2") format("woff2");
}
@font-face {
  font-family: "Inter";
  font-style: normal;
  font-weight: 500;
  font-display: block;
  src: url("./fonts/inter-medium.woff2") format("woff2");
}

:root {
  --bg: #0e0f12;
  --bg-elevated: #16181d;
  --text: #e8eaed;
  --text-muted: #9aa0a6;
  --accent: #7aa2f7;
  --danger: #f7768e;

  --s-1: 4px;
  --s-2: 8px;
  --s-3: 16px;
  --s-4: 32px;

  --t-body: 14px / 1.5 "Inter", system-ui, sans-serif;
  --t-ui: 13px / 1.4 "Inter", system-ui, sans-serif;
  --t-display: 20px / 1.3 "Inter", system-ui, sans-serif;
}

@media (prefers-color-scheme: light) {
  :root {
    --bg: #fafafa;
    --bg-elevated: #ffffff;
    --text: #1a1d21;
    --text-muted: #5f6368;
  }
}

html, body {
  background: var(--bg);
  color: var(--text);
  font: var(--t-body);
  margin: 0;
  padding: 0;
}
```

- [ ] **Step 2: Add Inter font files**

Download the OFL-licensed Inter woff2 subsets. The simplest path: fetch from `rsms/inter` GitHub releases, then copy `Inter-Regular.woff2` + `Inter-Medium.woff2` into the lib/fonts/ directory. If the build environment cannot fetch external assets, place placeholder zero-byte files and note in the CHANGELOG that real fonts must be dropped in before merge.

```bash
# In a network-capable environment:
curl -L -o /tmp/inter.zip https://github.com/rsms/inter/releases/download/v4.0/Inter-4.0.zip
unzip -j /tmp/inter.zip "Inter Web/Inter-Regular.woff2" -d crates/ui/src-svelte/src/lib/fonts/
unzip -j /tmp/inter.zip "Inter Web/Inter-Medium.woff2" -d crates/ui/src-svelte/src/lib/fonts/
mv crates/ui/src-svelte/src/lib/fonts/Inter-Regular.woff2 \
   crates/ui/src-svelte/src/lib/fonts/inter-regular.woff2
mv crates/ui/src-svelte/src/lib/fonts/Inter-Medium.woff2 \
   crates/ui/src-svelte/src/lib/fonts/inter-medium.woff2
```

If `curl` is unavailable or network-restricted, the executing engineer fetches the fonts manually and drops them into the directory before commit.

- [ ] **Step 3: Add the OFL license text**

`crates/ui/src-svelte/src/lib/fonts/OFL.txt` — copy verbatim from `https://openfontlicense.org/open-font-license-official-text/` or from the Inter release archive's accompanying `LICENSE.txt`.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/lib/tokens.css \
        crates/ui/src-svelte/src/lib/fonts/
git commit -m "ui-svelte: tokens.css + bundled Inter (OFL 1.1)"
```

---

### Task 21: `IpcClient` interface (TS)

**Files:**
- Create: `crates/ui/src-svelte/src/lib/ipc/client.ts`

- [ ] **Step 1: Write the interface**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Transport-agnostic IPC client. Components consume this interface
// only; the concrete `TauriTransport` lives in tauri.ts.

import type {
  Command,
  CommandResult,
  Event,
  EventFilter,
  IpcResponse,
} from "./types";

export interface IpcClient {
  /** Issue a one-shot command. Resolves with the wire response. */
  request(cmd: Command): Promise<IpcResponse>;

  /**
   * Open a long-lived event subscription matching `filter`. The
   * returned unsubscribe function closes the underlying channel.
   */
  subscribe(
    filter: EventFilter,
    onEvent: (e: Event) => void,
  ): Promise<() => void>;
}

/** Convenience: extract a `CommandResult` from a successful `IpcResponse`. */
export function unwrapOk(resp: IpcResponse): CommandResult {
  if ("Ok" in resp) return resp.Ok;
  throw new Error(`IPC error: ${JSON.stringify(resp)}`);
}
```

The exact `IpcResponse` discriminator key (`Ok` vs `resp` etc.) depends on what ts-rs emits for adjacent-tagged enums — adapt the destructuring once `types/IpcResponse.ts` is regenerated. Alternatively, write a small helper that handles both shapes.

- [ ] **Step 2: Commit**

```bash
git add crates/ui/src-svelte/src/lib/ipc/client.ts
git commit -m "ui-svelte: IpcClient TS interface"
```

---

### Task 22: `TauriTransport` (TS)

**Files:**
- Create: `crates/ui/src-svelte/src/lib/ipc/tauri.ts`

- [ ] **Step 1: Implement `TauriTransport`**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// TauriTransport: realises the IpcClient interface over Tauri 2 IPC.

import { invoke, Channel } from "@tauri-apps/api/core";

import type { IpcClient } from "./client";
import type { Command, Event, EventFilter, IpcResponse } from "./types";

export class TauriTransport implements IpcClient {
  async request(cmd: Command): Promise<IpcResponse> {
    return await invoke<IpcResponse>("ipc_request", { cmd });
  }

  async subscribe(
    filter: EventFilter,
    onEvent: (e: Event) => void,
  ): Promise<() => void> {
    const channel = new Channel<Event>();
    channel.onmessage = onEvent;
    await invoke("ipc_subscribe", { filter, channel });
    return () => {
      // Tauri Channel doesn't have an explicit close; the Rust side's
      // `tokio::spawn` loop exits when `channel.send` fails. Drop the
      // handler so further events are ignored.
      channel.onmessage = () => {};
    };
  }
}

export const ipcClient: IpcClient = new TauriTransport();
```

If the Tauri 2 release in use doesn't export `Channel` from `@tauri-apps/api/core` (older 2.x betas put it elsewhere), search the package's `.d.ts` files and fix the import path. The class shape is unchanged.

- [ ] **Step 2: Commit**

```bash
git add crates/ui/src-svelte/src/lib/ipc/tauri.ts
git commit -m "ui-svelte: TauriTransport realising IpcClient over Tauri 2"
```

---

### Task 23: Svelte stores

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/tor_status.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/daemon_info.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/contacts.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/conversation.ts`

- [ ] **Step 1: `tor_status.ts`**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { TorStatus } from "$lib/ipc/types";

export const torStatus = writable<TorStatus | null>(null);
```

- [ ] **Step 2: `daemon_info.ts`**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { CommandResult } from "$lib/ipc/types";

export const daemonInfo = writable<
  Extract<CommandResult, { result: "daemon_info" }> | null
>(null);
```

(Adjust the `Extract` discriminator key to match what ts-rs emits.)

- [ ] **Step 3: `contacts.ts`**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { ContactSummary } from "$lib/ipc/types";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";

export const contacts = writable<ContactSummary[]>([]);

export async function refreshContacts(): Promise<void> {
  const resp = await ipcClient.request({ cmd: "list_contacts" });
  const result = unwrapOk(resp);
  if ("Contacts" in result) {
    contacts.set(result.Contacts);
  }
}
```

(Discriminator keys depend on the ts-rs naming convention — adjust as needed.)

- [ ] **Step 4: `conversation.ts`**

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, get } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

interface ConversationState {
  contact: PublicKey | null;
  messages: MessageRecord[];
}

export const conversation = writable<ConversationState>({
  contact: null,
  messages: [],
});

export async function openConversation(contact: PublicKey): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact,
    limit: 200,
  });
  const result = unwrapOk(resp);
  const messages = "Messages" in result ? result.Messages : [];
  // newest-first from the daemon; reverse for chronological render.
  messages.reverse();
  conversation.set({ contact, messages });
}

export function appendMessage(record: MessageRecord): void {
  conversation.update((state) => {
    if (
      state.contact !== null &&
      record.contact === state.contact
    ) {
      return { ...state, messages: [...state.messages, record] };
    }
    return state;
  });
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/
git commit -m "ui-svelte: stores (tor_status, daemon_info, contacts, conversation)"
```

---

### Task 24: `TorPill.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/TorPill.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { torStatus } from "$lib/stores/tor_status";
</script>

<div class="pill" data-status={$torStatus ? Object.keys($torStatus)[0] : "idle"}>
  {#if $torStatus === null || ("Idle" in $torStatus)}
    <span class="dot grey" /> Disconnected
  {:else if "Bootstrapping" in $torStatus}
    <span class="dot grey" /> Connecting ({$torStatus.Bootstrapping}%)
  {:else if "Ready" in $torStatus}
    <span class="dot accent" /> Tor connected
  {:else if "Failed" in $torStatus}
    <span class="dot danger" title={$torStatus.Failed}>● Failed</span>
  {/if}
</div>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: var(--s-1);
    padding: var(--s-1) var(--s-2);
    background: var(--bg-elevated);
    border-radius: 999px;
    font: var(--t-ui);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    display: inline-block;
  }
  .grey { background: var(--text-muted); }
  .accent { background: var(--accent); }
  .danger { background: var(--danger); }
</style>
```

Adjust the `Object.keys` introspection if ts-rs emits adjacent-tagged enums with `{ event, data }` keys instead of single-key wrapping. The conditional logic above tracks the externally-tagged shape; adapt to whatever the generated TS actually produces.

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/TorPill.svelte
git commit -m "ui-svelte: TorPill component"
```

---

### Task 25: `ContactRow.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/ContactRow.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";

  let { summary, active = false, onclick }: {
    summary: ContactSummary;
    active?: boolean;
    onclick?: () => void;
  } = $props();

  function shortHash(pk: string): string {
    return pk.length > 8 ? pk.slice(0, 8) : pk;
  }

  function relativeTs(ts: number | null | undefined): string {
    if (!ts) return "";
    const now = Math.floor(Date.now() / 1000);
    const delta = now - ts;
    if (delta < 60) return `${delta}s`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h`;
    return new Date(ts * 1000).toLocaleDateString();
  }
</script>

<button class="row" class:active onclick={onclick}>
  <div class="title">
    {summary.nickname ?? shortHash(summary.pubkey)}
  </div>
  <div class="meta">
    <span class="preview">{summary.last_message_preview ?? ""}</span>
    <span class="ts">{relativeTs(summary.last_ts_recv)}</span>
  </div>
  {#if summary.unread_count > 0}
    <span class="badge">{summary.unread_count}</span>
  {/if}
</button>

<style>
  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--s-1);
    padding: var(--s-2) var(--s-3);
    background: transparent;
    border: none;
    text-align: left;
    color: var(--text);
    font: var(--t-body);
    cursor: pointer;
    width: 100%;
  }
  .row:hover, .row.active { background: var(--bg-elevated); }
  .title { font-weight: 500; }
  .meta { display: flex; justify-content: space-between; color: var(--text-muted); font: var(--t-ui); }
  .preview { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 200px; }
  .badge {
    background: var(--accent);
    color: var(--bg);
    border-radius: 999px;
    padding: 0 var(--s-1);
    font: var(--t-ui);
    align-self: center;
  }
</style>
```

Note Svelte 5 runes syntax (`$props()`). If using Svelte 4, substitute `export let summary: ContactSummary; export let active = false;`.

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/ContactRow.svelte
git commit -m "ui-svelte: ContactRow component"
```

---

### Task 26: `MessageBubble.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/MessageBubble.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { MessageRecord } from "$lib/ipc/types";

  let { record }: { record: MessageRecord } = $props();

  let body = $derived(
    record.kind && "Text" in record.kind ? record.kind.Text.body : "",
  );
  let isOutgoing = $derived(record.direction === "outgoing");
</script>

<div class="bubble" class:outgoing={isOutgoing}>
  <p class="body">{body}</p>
  <time class="ts">{new Date(record.ts_daemon_recv * 1000).toLocaleTimeString()}</time>
</div>

<style>
  .bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .body { margin: 0; white-space: pre-wrap; word-break: break-word; }
  .ts { color: var(--text-muted); font: var(--t-ui); display: block; margin-top: var(--s-1); }
</style>
```

(2.C ships outgoing styling only as a forward-compatible affordance; no outgoing rows surface in the UI until 2.D adds the composer.)

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/MessageBubble.svelte
git commit -m "ui-svelte: MessageBubble component"
```

---

### Task 27: `VirtualMessageList.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/VirtualMessageList.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import VirtualList from "svelte-virtual-list";
  import type { MessageRecord } from "$lib/ipc/types";
  import MessageBubble from "./MessageBubble.svelte";

  let { items }: { items: MessageRecord[] } = $props();
</script>

<div class="list">
  <VirtualList {items} let:item>
    <MessageBubble record={item} />
  </VirtualList>
</div>

<style>
  .list { height: 100%; overflow-y: auto; padding: var(--s-3); }
</style>
```

If `svelte-virtual-list`'s API is `slot let:item` it works as written. If we substituted a different lib (e.g., `@tanstack/svelte-virtual`), follow that lib's API.

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/VirtualMessageList.svelte
git commit -m "ui-svelte: VirtualMessageList component"
```

---

## Phase E — Wizard routes

### Task 28: `+layout.svelte` — root layout

**Files:**
- Create: `crates/ui/src-svelte/src/routes/+layout.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import "$lib/tokens.css";
  let { children } = $props();
</script>

{@render children()}
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/+layout.svelte
git commit -m "ui-svelte: +layout.svelte loads tokens.css"
```

---

### Task 29: `+page.svelte` — main shell with wizard redirect

**Files:**
- Create: `crates/ui/src-svelte/src/routes/+page.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";

  import TorPill from "$lib/components/TorPill.svelte";
  import ContactRow from "$lib/components/ContactRow.svelte";
  import VirtualMessageList from "$lib/components/VirtualMessageList.svelte";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { conversation, openConversation, appendMessage } from "$lib/stores/conversation";
  import { torStatus } from "$lib/stores/tor_status";
  import { daemonInfo } from "$lib/stores/daemon_info";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";

  onMount(async () => {
    const exists = await invoke<boolean>("vault_exists");
    if (!exists) {
      goto("/first-run");
      return;
    }
    // Existing-vault flow: prompt is part of /first-run for now (Step 2.5).
    // For 2.C, an existing vault still routes through the unlock screen.
    goto("/first-run");
  });

  async function selectContact(pubkey: string) {
    await openConversation(pubkey as any);
  }

  // Subscribe to events on mount; update stores.
  onMount(() => {
    let unsub: (() => void) | null = null;
    (async () => {
      unsub = await ipcClient.subscribe({ filter: "all" } as any, (e) => {
        if (typeof e === "object") {
          if ("TorStatusChanged" in e) torStatus.set(e.TorStatusChanged);
          else if ("MessageReceived" in e) appendMessage(e.MessageReceived.record);
        }
      });
    })();
    return () => { if (unsub) unsub(); };
  });
</script>

<div class="shell">
  <aside class="rail">
    {#each $contacts as c}
      <ContactRow
        summary={c}
        active={$conversation.contact === c.pubkey}
        onclick={() => selectContact(c.pubkey)}
      />
    {/each}
  </aside>
  <main class="pane">
    <header>
      <span class="title">{
        $contacts.find(c => c.pubkey === $conversation.contact)?.nickname
        ?? "Select a contact"
      }</span>
      <TorPill />
    </header>
    <VirtualMessageList items={$conversation.messages} />
  </main>
</div>

<style>
  .shell { display: grid; grid-template-columns: 280px 1fr; height: 100vh; }
  .rail { background: var(--bg); border-right: 1px solid var(--bg-elevated); overflow-y: auto; }
  .pane { display: flex; flex-direction: column; background: var(--bg); }
  header {
    display: flex; align-items: center; justify-content: space-between;
    padding: var(--s-3); border-bottom: 1px solid var(--bg-elevated);
  }
  .title { font: var(--t-display); }
</style>
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "ui-svelte: +page.svelte main shell"
```

---

### Task 30: First-run wizard wrapper

**Files:**
- Create: `crates/ui/src-svelte/src/routes/first-run/+page.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  import Welcome from "./Welcome.svelte";
  import Passphrase from "./Passphrase.svelte";
  import SeedPhrase from "./SeedPhrase.svelte";
  import Bootstrap from "./Bootstrap.svelte";

  let step = $state<"welcome" | "passphrase" | "seed" | "bootstrap" | "unlock">("welcome");
  let mnemonic = $state<string | null>(null);

  onMount(async () => {
    const exists = await invoke<boolean>("vault_exists");
    if (exists) step = "unlock";
  });

  function next(payload?: { mnemonic?: string }) {
    if (step === "welcome") step = "passphrase";
    else if (step === "passphrase") {
      mnemonic = payload?.mnemonic ?? null;
      step = "seed";
    } else if (step === "seed") step = "bootstrap";
  }
</script>

{#if step === "welcome"}
  <Welcome onNext={() => next()} />
{:else if step === "passphrase"}
  <Passphrase onNext={(m) => next({ mnemonic: m })} />
{:else if step === "seed"}
  <SeedPhrase {mnemonic} onNext={() => next()} />
{:else if step === "bootstrap"}
  <Bootstrap />
{:else if step === "unlock"}
  <Passphrase mode="unlock" onNext={() => (step = "bootstrap")} />
{/if}
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/first-run/+page.svelte
git commit -m "ui-svelte: first-run wizard wrapper"
```

---

### Task 31: `Welcome.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/routes/first-run/Welcome.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  let { onNext }: { onNext: () => void } = $props();
</script>

<section class="step">
  <h1>Welcome to Skattr</h1>
  <p>
    Skattr is a metadata-resistant peer-to-peer messenger. All traffic is
    routed over Tor v3 onion services. Messages are encrypted with MLS.
  </p>
  <h2>What this protects</h2>
  <ul>
    <li>Message contents (end-to-end encrypted, MLS).</li>
    <li>Network metadata (Tor onion routing).</li>
    <li>Server-side records (no central server).</li>
  </ul>
  <h2>What this does not protect</h2>
  <ul>
    <li>A compromised endpoint device.</li>
    <li>Side-channel attacks on your installation.</li>
    <li>Loss of your seed phrase or passphrase.</li>
  </ul>
  <button onclick={onNext}>Continue</button>
</section>

<style>
  .step { max-width: 60ch; margin: var(--s-4) auto; padding: var(--s-3); }
  button { background: var(--accent); color: var(--bg); padding: var(--s-2) var(--s-3); border: none; border-radius: 6px; cursor: pointer; font: var(--t-body); }
</style>
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/first-run/Welcome.svelte
git commit -m "ui-svelte: wizard step 1 (Welcome)"
```

---

### Task 32: `Passphrase.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/routes/first-run/Passphrase.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { zxcvbnAsync, zxcvbnOptions } from "@zxcvbn-ts/core";
  import * as enLang from "@zxcvbn-ts/language-en";

  zxcvbnOptions.setOptions({
    translations: enLang.translations,
    dictionary: enLang.dictionary,
  });

  let { mode = "create", onNext }: {
    mode?: "create" | "unlock";
    onNext: (mnemonic?: string) => void;
  } = $props();

  let pass = $state("");
  let confirm = $state("");
  let strength = $state<number>(0);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function evaluate() {
    if (!pass) { strength = 0; return; }
    const r = await zxcvbnAsync(pass);
    strength = r.score;
  }

  async function submit() {
    error = null;
    busy = true;
    try {
      if (mode === "create") {
        if (pass !== confirm) { error = "Passphrases don't match."; return; }
        if (strength < 3) { error = "Passphrase too weak (need at least 3/4)."; return; }
        const r = await invoke<{ mnemonic: string }>("identity_init", {
          args: { passphrase: pass, mnemonic: null },
        });
        await invoke("vault_unlock", { args: { passphrase: pass } });
        onNext(r.mnemonic);
      } else {
        await invoke("vault_unlock", { args: { passphrase: pass } });
        onNext();
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="step">
  <h1>{mode === "create" ? "Create a passphrase" : "Unlock"}</h1>
  <input
    type="password"
    placeholder="Passphrase"
    bind:value={pass}
    oninput={evaluate}
    autocomplete="new-password"
  />
  {#if mode === "create"}
    <input
      type="password"
      placeholder="Confirm"
      bind:value={confirm}
      autocomplete="new-password"
    />
    <div class="meter" data-strength={strength}>
      <span></span><span></span><span></span><span></span>
    </div>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  <button disabled={busy} onclick={submit}>
    {mode === "create" ? "Create identity" : "Unlock"}
  </button>
</section>

<style>
  .step { max-width: 40ch; margin: var(--s-4) auto; padding: var(--s-3); }
  input { display: block; width: 100%; margin: var(--s-2) 0; padding: var(--s-2); background: var(--bg-elevated); color: var(--text); border: 1px solid var(--text-muted); border-radius: 4px; font: var(--t-body); }
  .meter { display: flex; gap: 4px; margin-top: var(--s-2); }
  .meter span { flex: 1; height: 4px; background: var(--text-muted); border-radius: 2px; }
  .meter[data-strength="1"] span:nth-child(-n+1),
  .meter[data-strength="2"] span:nth-child(-n+2),
  .meter[data-strength="3"] span:nth-child(-n+3),
  .meter[data-strength="4"] span:nth-child(-n+4) { background: var(--accent); }
  .error { color: var(--danger); }
  button { background: var(--accent); color: var(--bg); padding: var(--s-2) var(--s-3); border: none; border-radius: 6px; cursor: pointer; font: var(--t-body); margin-top: var(--s-3); }
  button:disabled { opacity: 0.5; cursor: progress; }
</style>
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/first-run/Passphrase.svelte
git commit -m "ui-svelte: wizard step 2 (Passphrase, with zxcvbn meter)"
```

---

### Task 33: `SeedPhrase.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/routes/first-run/SeedPhrase.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  let { mnemonic, onNext }: {
    mnemonic: string | null;
    onNext: () => void;
  } = $props();

  let revealed = $state(false);
  let typeBack = $state("");
  let skipModal = $state(false);
  let error = $state<string | null>(null);

  function normalise(s: string): string[] {
    return s.toLowerCase().split(/\s+/).filter(Boolean);
  }

  function confirm() {
    if (!mnemonic) { error = "no mnemonic"; return; }
    const expected = normalise(mnemonic);
    const got = normalise(typeBack);
    if (expected.length !== got.length || expected.some((w, i) => w !== got[i])) {
      error = `Confirmation failed (expected ${expected.length} words in order).`;
      return;
    }
    onNext();
  }
</script>

<section class="step">
  <h1>Save your seed phrase</h1>
  <p class="warn">
    These 24 words are the only way to restore your identity. Skattr cannot
    recover them. Write them down somewhere safe before continuing.
  </p>
  {#if !revealed}
    <button onclick={() => (revealed = true)}>Reveal seed phrase</button>
  {:else if mnemonic}
    <pre class="seed">{mnemonic}</pre>
    <p>Type your seed phrase back to confirm.</p>
    <textarea
      bind:value={typeBack}
      placeholder="word1 word2 word3 …"
      rows="4"
    ></textarea>
    {#if error}<p class="error">{error}</p>{/if}
    <button onclick={confirm}>Confirm</button>
    <button class="link" onclick={() => (skipModal = true)}>I've written it down — skip type-back</button>
  {/if}
  {#if skipModal}
    <div class="modal">
      <div class="modal-body">
        <h2>Are you sure?</h2>
        <p class="warn">
          You will not be able to verify the seed phrase you wrote down.
          If you lose it, your identity is unrecoverable. Skattr will not
          ask again.
        </p>
        <button onclick={onNext}>Yes, skip confirmation</button>
        <button onclick={() => (skipModal = false)}>Cancel</button>
      </div>
    </div>
  {/if}
</section>

<style>
  .step { max-width: 60ch; margin: var(--s-4) auto; padding: var(--s-3); }
  .warn { color: var(--danger); }
  .seed {
    background: var(--bg-elevated);
    padding: var(--s-3);
    border-radius: 6px;
    user-select: all;
    word-spacing: var(--s-1);
    font: var(--t-body);
  }
  textarea {
    width: 100%;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--text-muted);
    border-radius: 4px;
    padding: var(--s-2);
    font: var(--t-body);
  }
  .error { color: var(--danger); }
  button {
    background: var(--accent); color: var(--bg);
    padding: var(--s-2) var(--s-3);
    border: none; border-radius: 6px; cursor: pointer;
    font: var(--t-body); margin: var(--s-2) var(--s-2) 0 0;
  }
  .link { background: transparent; color: var(--text-muted); padding: 0; }
  .modal {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid; place-items: center; z-index: 100;
  }
  .modal-body {
    background: var(--bg-elevated);
    padding: var(--s-3);
    border: 2px solid var(--danger);
    border-radius: 8px;
    max-width: 50ch;
  }
</style>
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/first-run/SeedPhrase.svelte
git commit -m "ui-svelte: wizard step 3 (SeedPhrase, type-back + skip modal)"
```

---

### Task 34: `Bootstrap.svelte`

**Files:**
- Create: `crates/ui/src-svelte/src/routes/first-run/Bootstrap.svelte`

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";

  import { ipcClient } from "$lib/ipc/tauri";
  import { torStatus } from "$lib/stores/tor_status";
  import { refreshContacts } from "$lib/stores/contacts";
  import { daemonInfo } from "$lib/stores/daemon_info";
  import { unwrapOk } from "$lib/ipc/client";

  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      await invoke("start_in_process_cmd");
      // Subscribe TorStatus first so the cached replay paints the pill.
      await ipcClient.subscribe({ filter: "tor_status" } as any, (e: any) => {
        if (e && "TorStatusChanged" in e) {
          torStatus.set(e.TorStatusChanged);
          if ("Ready" in e.TorStatusChanged) finishBootstrap();
        }
      });
    } catch (e) {
      error = String(e);
    }
  });

  async function finishBootstrap() {
    try {
      const info = await ipcClient.request({ cmd: "daemon_info" });
      const r = unwrapOk(info);
      if ("DaemonInfo" in r) daemonInfo.set(r as any);
      await refreshContacts();
      goto("/");
    } catch (e) {
      error = String(e);
    }
  }
</script>

<section class="step">
  <h1>Connecting to Tor</h1>
  {#if error}
    <p class="error">{error}</p>
  {:else if $torStatus && "Bootstrapping" in $torStatus}
    <progress max="100" value={$torStatus.Bootstrapping} />
    <p>{$torStatus.Bootstrapping}%</p>
  {:else if $torStatus && "Ready" in $torStatus}
    <p>Connected. Loading…</p>
  {:else}
    <p>Starting…</p>
  {/if}
</section>

<style>
  .step { max-width: 40ch; margin: var(--s-4) auto; padding: var(--s-3); text-align: center; }
  progress { width: 100%; }
  .error { color: var(--danger); }
</style>
```

- [ ] **Step 1: Commit**

```bash
git add crates/ui/src-svelte/src/routes/first-run/Bootstrap.svelte
git commit -m "ui-svelte: wizard step 4 (Bootstrap)"
```

---

## Phase F — Tests

### Task 35: Vitest snapshot for `tokens.css`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/tokens.css.test.ts`

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Regression guard: tokens.css must not drift without an explicit
// design-system change. Update this snapshot intentionally only.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

describe("tokens.css", () => {
  it("matches the locked palette", () => {
    const path = resolve(__dirname, "tokens.css");
    const contents = readFileSync(path, "utf8");
    expect(contents).toContain("--bg: #0e0f12");
    expect(contents).toContain("--bg-elevated: #16181d");
    expect(contents).toContain("--text: #e8eaed");
    expect(contents).toContain("--text-muted: #9aa0a6");
    expect(contents).toContain("--accent: #7aa2f7");
    expect(contents).toContain("--danger: #f7768e");
    expect(contents).toContain("--s-1: 4px");
    expect(contents).toContain("--s-4: 32px");
    expect(contents).toContain("--t-body: 14px / 1.5");
  });
});
```

- [ ] **Step 1: Run**

```bash
cd crates/ui/src-svelte && pnpm test
```

Expected: PASS.

- [ ] **Step 2: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/src/lib/tokens.css.test.ts
git commit -m "ui-svelte: tokens.css regression snapshot"
```

---

### Task 36: Vitest contract test for `IpcClient`

**Files:**
- Create: `crates/ui/src-svelte/src/lib/ipc/client.test.ts`

```typescript
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, expect, it, vi } from "vitest";
import { unwrapOk } from "./client";

describe("unwrapOk", () => {
  it("returns the inner CommandResult on Ok", () => {
    const resp = { Ok: { result: "ok" } } as any;
    expect(unwrapOk(resp)).toEqual({ result: "ok" });
  });

  it("throws on Err", () => {
    const resp = { Err: { err: "internal", data: "boom" } } as any;
    expect(() => unwrapOk(resp)).toThrow();
  });
});

describe("TauriTransport", () => {
  it("invokes ipc_request with the cmd payload", async () => {
    vi.mock("@tauri-apps/api/core", () => ({
      invoke: vi.fn().mockResolvedValue({ Ok: { result: "ok" } }),
      Channel: class {
        onmessage = () => {};
      },
    }));
    const { TauriTransport } = await import("./tauri");
    const { invoke } = await import("@tauri-apps/api/core");
    const t = new TauriTransport();
    const r = await t.request({ cmd: "list_contacts" } as any);
    expect(invoke).toHaveBeenCalledWith("ipc_request", {
      cmd: { cmd: "list_contacts" },
    });
    expect(r).toEqual({ Ok: { result: "ok" } });
  });
});
```

- [ ] **Step 1: Run**

```bash
cd crates/ui/src-svelte && pnpm test
```

Expected: PASS.

- [ ] **Step 2: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/src/lib/ipc/client.test.ts
git commit -m "ui-svelte: Vitest contract for IpcClient + TauriTransport"
```

---

### Task 37: Playwright first-run wizard happy path

**Files:**
- Create: `crates/ui/src-svelte/tests/e2e/first-run.spec.ts`
- Create: `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` (test-only mock)

- [ ] **Step 1: Tauri mock helper**

`crates/ui/src-svelte/src/lib/test/tauri-mock.ts`:

```typescript
// Test-only: mocks the @tauri-apps/api surface for headless Playwright.
// Loaded via Vite alias when TAURI_MOCK=1.

let _vault = false;
const mockMnemonic = "abandon ability able about above absent absorb abstract absurd abuse access accident account accuse achieve acid acoustic acquire across act action active activity actor actress";

export const invoke = async (cmd: string, payload?: any) => {
  switch (cmd) {
    case "vault_exists": return _vault;
    case "identity_init":
      _vault = true;
      return { mnemonic: mockMnemonic };
    case "vault_unlock": return null;
    case "start_in_process_cmd":
      return { onion: "abcd.onion", ipc_socket: "/tmp/skattr.sock" };
    case "ipc_request":
      if (payload.cmd.cmd === "daemon_info") {
        return { Ok: { result: "daemon_info", data: {
          local_pubkey: "00".repeat(32),
          current_onion: "abcd.onion",
          daemon_version: "0.0.1",
          schema_version: 9,
        } } };
      }
      if (payload.cmd.cmd === "list_contacts") {
        return { Ok: { result: "contacts", data: [] } };
      }
      return { Ok: { result: "ok" } };
    case "ipc_subscribe":
      return null;
  }
  throw new Error(`unmocked: ${cmd}`);
};

export class Channel<T> {
  onmessage: (e: T) => void = () => {};
}
```

In `vite.config.ts`, conditionally alias `@tauri-apps/api/core` to this mock when `process.env.TAURI_MOCK === "1"`:

```typescript
// vite.config.ts (extended)
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [sveltekit()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_"],
  resolve:
    process.env.TAURI_MOCK === "1"
      ? { alias: { "@tauri-apps/api/core": "/src/lib/test/tauri-mock.ts" } }
      : undefined,
});
```

- [ ] **Step 2: First-run Playwright spec**

`crates/ui/src-svelte/tests/e2e/first-run.spec.ts`:

```typescript
import { expect, test } from "@playwright/test";

test("first-run wizard happy path", async ({ page }) => {
  await page.goto("/first-run");
  await expect(page.getByRole("heading", { name: "Welcome to Skattr" })).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();

  // Passphrase step
  await page.getByPlaceholder("Passphrase").fill("correct horse battery staple");
  await page.getByPlaceholder("Confirm").fill("correct horse battery staple");
  await page.getByRole("button", { name: "Create identity" }).click();

  // Seed-phrase step — type-back
  await page.getByRole("button", { name: "Reveal seed phrase" }).click();
  const seed = await page.locator("pre.seed").innerText();
  await page.locator("textarea").fill(seed);
  await page.getByRole("button", { name: "Confirm" }).click();

  // Bootstrap step — mock immediately reports Ready.
  await expect(page).toHaveURL("/", { timeout: 10_000 });
});
```

- [ ] **Step 3: Update `package.json` scripts**

Add to `package.json`:

```json
"test:e2e": "TAURI_MOCK=1 playwright test"
```

(Replacing the existing `test:e2e` line.)

- [ ] **Step 4: Run**

```bash
cd crates/ui/src-svelte
pnpm exec playwright install chromium
TAURI_MOCK=1 pnpm test:e2e
```

Expected: PASS. If the bootstrap step's mock doesn't drive a `Ready` event, extend the mock to fire one synthetically; this is acceptable for the headless-mock test only.

- [ ] **Step 5: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/src/lib/test/tauri-mock.ts \
        crates/ui/src-svelte/tests/e2e/first-run.spec.ts \
        crates/ui/src-svelte/vite.config.ts \
        crates/ui/src-svelte/package.json
git commit -m "ui-svelte: Playwright first-run wizard happy path (Tauri mock)"
```

---

### Task 38: Playwright unlock path

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` (add toggle for pre-existing vault)
- Create: `crates/ui/src-svelte/tests/e2e/unlock.spec.ts`

- [ ] **Step 1: Extend mock**

In `tauri-mock.ts`, accept a query parameter or env var `TAURI_MOCK_VAULT_EXISTS=1` to flip `_vault = true` on load:

```typescript
let _vault = (typeof window !== "undefined" && new URL(window.location.href).searchParams.get("vault") === "yes")
  || (typeof process !== "undefined" && process.env?.TAURI_MOCK_VAULT_EXISTS === "1");
```

- [ ] **Step 2: Spec**

```typescript
import { expect, test } from "@playwright/test";

test("existing-vault unlock path", async ({ page }) => {
  await page.goto("/?vault=yes");
  // Wizard wrapper redirects to unlock screen.
  await page.getByPlaceholder("Passphrase").fill("correct horse battery staple");
  await page.getByRole("button", { name: "Unlock" }).click();
  await expect(page).toHaveURL("/", { timeout: 10_000 });
});
```

- [ ] **Step 3: Run**

```bash
cd crates/ui/src-svelte && TAURI_MOCK=1 pnpm test:e2e unlock
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cd ../../..
git add crates/ui/src-svelte/src/lib/test/tauri-mock.ts \
        crates/ui/src-svelte/tests/e2e/unlock.spec.ts
git commit -m "ui-svelte: Playwright unlock path"
```

---

### Task 39: Rust integration test in `crates/tests/`

**Files:**
- Create: `crates/tests/src/ui_first_run.rs`
- Modify: `crates/tests/src/lib.rs` to declare the new module if needed

- [ ] **Step 1: Test**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.C: end-to-end first-run integration. Bootstraps a real
//! Tor-backed daemon via `Daemon::run`, drives `Command::DaemonInfo`
//! and `Command::ListContacts` over the IPC socket, and asserts the
//! Subscribe TorStatus replay surfaces. `#[ignore]`-gated; run with
//! `cargo test -p skattr-tests --release -- --ignored ui_first_run`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use zeroize::Zeroizing;

use skattr_core::daemon::commands::{Command, CommandResult};
use skattr_core::daemon::events::{Event, TorStatus};
use skattr_core::daemon::ipc::wire::{EventFilter, IpcResponse};
use skattr_core::daemon::ipc::IpcClient;
use skattr_core::daemon::{Config, Daemon};
use skattr_core::identity::{IdentityKey, Seed, Vault};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns real Arti; run with --ignored"]
async fn ui_first_run_daemon_info_and_subscribe_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let pw = Zeroizing::new("ui-first-run-passphrase".to_string());

    // Pre-stage a vault as if the wizard's identity_init step had run.
    let seed = Seed::generate().unwrap();
    let key = IdentityKey::from_seed(&seed).unwrap();
    Vault::create(&data_dir.join("identity.vault"), key, pw.as_str()).unwrap();

    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.clone();
    config.ipc_socket = Some(data_dir.join("ipc.sock"));

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_fut = async move { let _ = shutdown_rx.await; };

    let dd = data_dir.clone();
    let pw_for_run = pw.clone();
    let task = tokio::spawn(async move {
        Daemon::run(&dd, &pw_for_run, config, ready_tx, shutdown_fut).await
    });

    let ready = tokio::time::timeout(Duration::from_secs(180), ready_rx)
        .await.expect("ready in 180s").expect("ready_tx open");

    // Connect IPC client; issue Command::DaemonInfo.
    let mut req_client = IpcClient::connect(&ready.ipc_socket).await.unwrap();
    let info = req_client.execute(Command::DaemonInfo).await.unwrap();
    match info {
        CommandResult::DaemonInfo {
            current_onion,
            daemon_version,
            schema_version,
            ..
        } => {
            assert_eq!(current_onion.as_deref(), Some(ready.onion.as_str()));
            assert_eq!(daemon_version, env!("CARGO_PKG_VERSION"));
            assert!(schema_version >= 9);
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }

    // Connect a separate client; subscribe TorStatus; expect replay frame.
    let mut sub_client = IpcClient::connect(&ready.ipc_socket).await.unwrap();
    sub_client.subscribe(EventFilter::TorStatus).await.unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), sub_client.next_event())
        .await.expect("event within 5s").expect("event ok");
    assert!(matches!(ev, Event::TorStatusChanged(TorStatus::Ready)));

    // ListContacts should return empty for a fresh vault.
    let lc = req_client.execute(Command::ListContacts).await.unwrap();
    assert!(matches!(lc, CommandResult::Contacts(v) if v.is_empty()));

    // Shutdown.
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task).await;
}
```

- [ ] **Step 2: Module declaration**

If `crates/tests/src/lib.rs` uses module-style aggregation (look at how other integration tests are wired), add:

```rust
#[cfg(all(test, feature = "test-harness"))]
mod ui_first_run;
```

If existing integration tests are file-per-test (e.g., `delivery_kill_mid_message.rs` directly under `crates/tests/src/`), the new file just lives next to them.

- [ ] **Step 3: Run**

```bash
cargo test -p skattr-tests --release -- --ignored ui_first_run
```

Expected: PASS (3+ minutes including Tor bootstrap; can be skipped in CI without `--ignored`).

- [ ] **Step 4: Commit**

```bash
git add crates/tests/src/ui_first_run.rs crates/tests/src/lib.rs
git commit -m "tests: Phase 2.C ui_first_run integration (real Tor, ignore-gated)"
```

---

## Phase G — Verification + docs

### Task 40: Run full Rust verification

- [ ] **Step 1: Format**

```bash
cargo fmt --all -- --check
```

Expected: clean. If not, `cargo fmt --all` and amend the relevant commit (no unrelated commits).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: zero warnings.

- [ ] **Step 3: Tests (default)**

```bash
cargo test --workspace
```

Expected: all PASS (ignored tests skipped).

- [ ] **Step 4: Tests (with test-harness feature)**

```bash
cargo test --workspace --features test-harness
```

Expected: all PASS.

- [ ] **Step 5: deny check**

```bash
cargo deny check
```

Expected: zero issues. New deps (`ts-rs`, `parking_lot`, `tauri`, `tauri-build`) must satisfy the existing license allowlist.

- [ ] **Step 6: Commit any fixes**

If any check failed and fixes were applied, commit them with a descriptive message. If clean, no commit needed for this task.

---

### Task 41: Run TS verification

- [ ] **Step 1: TypeScript check**

```bash
cd crates/ui/src-svelte && pnpm check
```

Expected: zero errors.

- [ ] **Step 2: Vitest**

```bash
pnpm test
```

Expected: all PASS.

- [ ] **Step 3: Playwright (mocked)**

```bash
TAURI_MOCK=1 pnpm test:e2e
```

Expected: all PASS.

- [ ] **Step 4: Build**

```bash
pnpm build
```

Expected: clean SvelteKit static build emitting `build/`.

- [ ] **Step 5: No commit unless fixes were needed.**

---

### Task 42: CHANGELOG entry

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add entry**

At the top of `CHANGELOG.md` under an `## Unreleased` section (creating one if absent):

```markdown
## Unreleased

### Phase 2.C — UI bootstrap (read-only conversation MVP)

- New crate `crates/ui/` (GPLv3): Tauri 2 + SvelteKit shell with
  in-process `Daemon::run`, two-phase Tauri command surface
  (pre-daemon `vault_exists` / `identity_init` / `vault_unlock`,
  post-daemon `ipc_request` / `ipc_subscribe`).
- New wire surface (additive only): `Command::DaemonInfo` +
  `CommandResult::DaemonInfo`; `ContactSummary` projection extensions
  (`unread_count`, `last_message_preview`, `last_ts_recv`, all
  `#[serde(default)]`); `Subscribe` ack now replays the cached
  `TorStatusChanged` event for `EventFilter::All` and
  `EventFilter::TorStatus`.
- `Pool::schema_version()` exposed; `MessageRepo::latest_for_group()`
  added; `dispatch::list_contacts` populates the new fields and
  applies `last_ts_recv DESC NULLS LAST, added_at DESC` ordering.
- First-run wizard: welcome → passphrase (zxcvbn ≥3) → seed phrase
  type-back confirm (24-word BIP39) → Tor bootstrap.
- Locked design tokens (`tokens.css`) and bundled Inter font (OFL 1.1).
- 2.C-only behaviour: closing the window quits the daemon. 2.F
  upgrades this to hide-to-tray; CLI users running the UI alongside
  should be aware.
- Mailbox wire surface inherited unchanged from 2.B; UI does not
  render mailbox state in 2.C (2.F).
- `ts-rs` codegen: every wire type derives `TS`; SvelteKit consumes
  via `crates/ui/src-svelte/src/lib/ipc/types/`.
- New integration test `crates/tests/src/ui_first_run.rs`,
  `#[ignore]`-gated, exercises the full bootstrap path.
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: Phase 2.C CHANGELOG entry"
```

---

### Task 43: Update CLAUDE.md "Repository state"

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Replace the opening paragraph of the "Repository state" section**

In `CLAUDE.md`, find the paragraph beginning "Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24)…" and update the opening sentence to:

```
**Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24);
Phase 2.A (mailbox server) is complete; Phase 2.B (mailbox client +
ContactCard rotation) is complete (merged 2026-05-01); Phase 2.C
(UI bootstrap, read-only conversation MVP) is complete (merged
YYYY-MM-DD).**
```

(Replace `YYYY-MM-DD` with the actual merge date when the PR lands. For now use today's date in `2026-05-DD` form.)

Append a new paragraph after the Phase 2.B summary:

```
Phase 2.C added a new `crates/ui/` crate (GPLv3): Tauri 2 +
SvelteKit shell that boots an in-process `Daemon::run`, walks
first-run users through a four-step wizard (welcome → passphrase
→ 24-word BIP39 seed type-back → Tor bootstrap), and renders a
read-only contact list + open conversation with live-append on
`Event::MessageReceived`. New wire surface (additive only):
`Command::DaemonInfo`, `ContactSummary` projection fields
(`unread_count`, `last_message_preview`, `last_ts_recv`), and a
filter-gated `TorStatusChanged` replay on the `Subscribe` ack
backed by a tap task on `DaemonHandle::latest_tor_status`. `ts-rs`
emits TS bindings for every wire type into
`crates/ui/src-svelte/src/lib/ipc/types/` (gitignored; regenerated
via `cargo test -p skattr-core`). 2.C closes the window by quitting
the daemon — 2.F replaces this with hide-to-tray. The mailbox CRUD
wire surface from 2.B is consumed unchanged; UI rendering of
mailbox state lands in 2.F.
```

Update the "next workstream" line:

```
The next workstream is Phase 2.D (conversation view: composer,
delivery state icons, scroll-back paginatation; depends on 2.C) —
see `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
for the Phase 2 decomposition and
`docs/superpowers/specs/2026-05-01-phase-2c-ui-bootstrap-design.md`
for the 2.C internals.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md status update — Phase 2.C complete"
```

---

### Task 44: Final pre-merge sanity sweep

- [ ] **Step 1: Re-run the full Rust sweep**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --features test-harness
cargo deny check
```

Expected: all green.

- [ ] **Step 2: Re-run the SvelteKit sweep**

```bash
cd crates/ui/src-svelte
pnpm check
pnpm test
TAURI_MOCK=1 pnpm test:e2e
pnpm build
cd ../../..
```

Expected: all green.

- [ ] **Step 3: Real-Tor smoke test (optional, time-permitting)**

```bash
cargo test -p skattr-tests --release -- --ignored ui_first_run
```

Expected: PASS within ~3–5 minutes including Tor bootstrap.

- [ ] **Step 4: Verify gitignore is doing its job**

```bash
git status --short
```

Expected: clean working tree (no untracked `crates/ui/src-svelte/src/lib/ipc/types/*.ts` files).

- [ ] **Step 5: No commit unless something broke.**

---

### Task 45: Open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin phase-2c-ui-bootstrap
```

- [ ] **Step 2: Open PR via gh**

```bash
gh pr create --title "Phase 2.C: UI bootstrap (read-only conversation MVP)" --body "$(cat <<'EOF'
## Summary
- New `crates/ui/` Tauri 2 + SvelteKit crate with in-process `Daemon::run` and two-phase Tauri command surface (pre-daemon wizard / post-daemon IPC bridge).
- Additive wire surface: `Command::DaemonInfo`, `ContactSummary` projections (`unread_count` / `last_message_preview` / `last_ts_recv`), filter-gated `TorStatus` replay on `Subscribe` ack.
- First-run wizard (welcome → passphrase → 24-word seed type-back → Tor bootstrap), locked design tokens, bundled Inter, virtualised message list. 2.C is read-only by design.

## Test plan
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] `cargo test --workspace --features test-harness` green
- [ ] `cargo deny check` clean
- [ ] `pnpm check` / `pnpm test` / `pnpm build` green in `crates/ui/src-svelte/`
- [ ] `TAURI_MOCK=1 pnpm test:e2e` green
- [ ] `cargo test -p skattr-tests --release -- --ignored ui_first_run` green (real Tor; optional)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 2: Surface the PR URL** for review.

---

## Self-review notes

- All 13 locked decisions in the spec map to a task. (1) wizard-first → Tasks 14–16, 30–34. (2) mailbox surface inheritance → no task; documented. (3) quit-on-close → Task 14 main.rs hook. (4) Subscribe replay → Tasks 5–7. (5) `Command::DaemonInfo` → Tasks 3, 8. (6) `ContactSummary` extensions → Tasks 4, 9. (7) ordering → Task 9. (8) live-append → Task 29 onMount. (9) Inter → Task 20. (10) `svelte-virtual-list` → Task 19 + 27. (11) wizard granularity + type-back → Tasks 30–34. (12) tokens → Task 20. (13) `.gitignored` types → Task 12.
- Wire-format contract covered by Tasks 3, 4, 8, 9 (additions), 7 (Subscribe replay), 2 (storage accessor), 1 (`Pool::schema_version`).
- Test plan covered by Tasks 35–39.
- CHANGELOG + CLAUDE.md updates covered by Tasks 42, 43.

---

## Execution handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-01-phase-2c-ui-bootstrap.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
