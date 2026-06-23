# Phase 4.D — T3 Security Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land nine small, independent security/correctness hardening fixes (five kept audit cluster-6 findings + four pulled-forward fixes), with no protocol change and no new feature.

**Architecture:** Each item is a self-contained edit to existing code with its own TDD unit test. Items are independent (disjoint files) and ordered by blast radius: the migration runner and the attachment-completion gate first, then the security-sensitive core items, then the mechanical ones.

**Tech Stack:** Rust (skattr-core, skattr-ui), SvelteKit/TypeScript (skattr-ui frontend), `rusqlite`, `thiserror`, `zeroize`, `tracing`, `tauri-plugin-opener`, `libc`.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-23-phase-4d-t3-hardening-design.md`. The HS-key "salt" finding is **dropped** (premise false — `age` scrypt salts per-file); do not implement it.
- **No `unwrap()`/`expect()` in library/command bodies** — return typed `Result`, map errors. `#[cfg(test)]` may use them under the file's existing `#[allow(...)]`.
- **License header on every edited/new file** (GPLv3): Rust `// SPDX-License-Identifier: GPL-3.0-or-later` + `// Copyright (C) 2026 Myggiz AB`; Svelte/TS the same in `//` form. (All target files already have headers — do not duplicate.)
- **Cross-platform:** item 3 (0700) and item 5 (getuid) edits are `#[cfg(unix)]`; Windows behavior unchanged.
- **Toolchain:** run Rust gates under pinned **1.95.0** (`rustup override set 1.95.0` in the repo; the default floats to 1.96 which SIGSEGVs the arti tree). CI uses its own stable.
- **Local pre-push gates (all must pass):** `cargo fmt --all --check`, `cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings`, `cargo test -p skattr-core -p skattr-ui`, plus `pnpm test` + `pnpm check` for the two UI tasks. **`pnpm check` stays local (not added to CI here — that's 4.A).**
- **Regression safety:** all existing core tests + the Phase-3 attachment guardrails + Vitest + e2e must stay green. Highest care: Task 1 (migration runner — every DB open) and Task 2 (attachment completion path).
- **Review tiers (subagent-driven):** Tasks 2, 3, 4, 5, 6 (P2, item 1, item 2, item 5, P1 — security/auth/secret/integrity sensitive) → **opus review**; Tasks 1, 7, 8, 9 (item 4-impl, item 3, P3, P5) → **standard review** (Task 1 still merits careful review for data-integrity).

## File Structure

| Task | Item | Files |
|---|---|---|
| 1 | Item 4 — schema-downgrade guard | `crates/core/src/storage/error_kind.rs`, `crates/core/src/storage/migrations.rs` |
| 2 | P2 — completion CAS gate | `crates/core/src/storage/attachments.rs`, `crates/core/src/delivery/peer.rs`, `crates/core/src/daemon/inbound.rs` |
| 3 | Item 1 — log-redaction bypass | `crates/core/src/daemon/dispatch.rs` |
| 4 | Item 2 — Zeroizing secret CBOR | `crates/core/src/identity/key.rs`, `crates/core/src/mls/key_package.rs` |
| 5 | Item 5 — getuid uid fallback | `crates/core/src/daemon/ipc/server/unix.rs` |
| 6 | P1 — opener path confinement | `crates/ui/src/attachments.rs`, `crates/ui/src/main.rs` |
| 7 | Item 3 — data_dir 0700 | `crates/core/src/storage/pool.rs` |
| 8 | P3 — chunk_sweep warn logs | `crates/core/src/delivery/chunk_sweep.rs` |
| 9 | P5 — PromotedMessage cast | `crates/ui/src-svelte/src/lib/stores/conversation.ts` |

---

## Task 1: Item 4 — Schema-downgrade guard

**Files:**
- Modify: `crates/core/src/storage/error_kind.rs` (add `SchemaTooNew` variant)
- Modify: `crates/core/src/storage/migrations.rs` (guard in `apply` + test)

**Interfaces:**
- Produces: `StorageErrorKind::SchemaTooNew { found: u32, max_known: u32 }`; `migrations::apply` returns `Err(CoreError::Storage(StorageErrorKind::SchemaTooNew{..}))` when the DB's `schema_version` exceeds the newest known migration.

- [ ] **Step 1: Add the error variant**

In `crates/core/src/storage/error_kind.rs`, add this variant inside the `StorageErrorKind` enum (after `Other(String)`, keeping the existing `#[error(...)]` style):
```rust
    /// The DB `schema_version` is newer than this binary knows about — an
    /// older binary opened a DB written by a newer one. Refuse rather than
    /// silently operating on an unknown schema. Projects to
    /// `DaemonErrorKind::StorageError` (no new wire variant).
    #[error("schema too new: db at version {found}, this binary knows up to {max_known}")]
    SchemaTooNew { found: u32, max_known: u32 },
```
(The enum is `#[non_exhaustive]`, so adding a variant is non-breaking. `CoreError::kind()` already maps `StorageErrorKind` arms — `SchemaTooNew` falls through to the existing `StorageError` projection; no edit needed there since the catch-all maps non-FtsSyntax to `StorageError`. If `kind()` matches arms exhaustively, add a `SchemaTooNew => DaemonErrorKind::StorageError` arm.)

- [ ] **Step 2: Write the failing test**

In `crates/core/src/storage/migrations.rs`, add to the `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn refuses_db_newer_than_binary() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap(); // bring to latest known
        let max = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [max + 1],
        )
        .unwrap();
        let err = apply(&mut conn).unwrap_err();
        assert!(matches!(
            err,
            crate::error::CoreError::Storage(
                crate::storage::StorageErrorKind::SchemaTooNew { .. }
            )
        ));
    }
```

- [ ] **Step 3: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core migrations::tests::refuses_db_newer 2>&1 | tail -15`
Expected: FAIL — `apply` returns `Ok` (no guard yet), so `unwrap_err()` panics.

- [ ] **Step 4: Implement the guard**

In `crates/core/src/storage/migrations.rs`, in `pub(crate) fn apply`, immediately after the `let current: u32 = …unwrap_or(0);` block and **before** the `for m in ALL_MIGRATIONS` loop, insert:
```rust
    let max_known = ALL_MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0);
    if current > max_known {
        return Err(crate::error::CoreError::Storage(
            crate::storage::StorageErrorKind::SchemaTooNew {
                found: current,
                max_known,
            },
        ));
    }
```
(Uses fully-qualified paths so no new `use` is required; if `CoreError`/`StorageErrorKind` are already imported in the file, the short names work too.)

- [ ] **Step 5: Run test, verify it passes + regression**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core migrations:: 2>&1 | tail -20`
Expected: `refuses_db_newer_than_binary` PASS and all existing `migrations::tests::*` still PASS (the guard only triggers when `current > max_known`, never on normal upgrades).

- [ ] **Step 6: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/storage/error_kind.rs crates/core/src/storage/migrations.rs
git commit -m "fix(4.D): refuse opening a DB newer than the binary (schema-downgrade guard)"
```

---

## Task 2: P2 — Attachment completion compare-and-set gate

**Files:**
- Modify: `crates/core/src/storage/attachments.rs` (add `set_status_if_pending` + test)
- Modify: `crates/core/src/delivery/peer.rs` (`finalize_rx` — gate the emit)
- Modify: `crates/core/src/daemon/inbound.rs` (`finalize_offline` — gate the emit)

**Interfaces:**
- Produces: `AttachmentRepo::set_status_if_pending(&self, attachment_id: &[u8; 16]) -> Result<bool>` — atomically flips `status` `pending → complete`, returns `true` iff this call performed the transition (rows_affected == 1). Used as the single fire-gate for `AttachmentReceived`.

- [ ] **Step 1: Write the failing repo test**

In `crates/core/src/storage/attachments.rs`, add to the `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn set_status_if_pending_fires_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        let pool = crate::storage::Pool::open(dir.path(), &seed).unwrap();
        let aid = [0x11u8; 16];
        // Insert a pending 'in' attachment row directly (schema from 0015).
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO attachments \
                 (attachment_id, direction, manifest, total_chunks, status, created_at) \
                 VALUES (?1, 'in', x'00', 1, 'pending', 0)",
                rusqlite::params![&aid[..]],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
        let repo = AttachmentRepo::new(&pool);
        assert!(repo.set_status_if_pending(&aid).unwrap(), "first call wins");
        assert!(!repo.set_status_if_pending(&aid).unwrap(), "second call loses");
    }
```
> If the file's existing tests use a different Pool constructor, mirror theirs; `Pool::open(dir, &seed)` is the public signature (`storage/pool.rs`).

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core attachments::tests::set_status_if_pending 2>&1 | tail -15`
Expected: FAIL — method `set_status_if_pending` not found.

- [ ] **Step 3: Implement the CAS method**

In `crates/core/src/storage/attachments.rs`, add to `impl<'p> AttachmentRepo<'p>` (next to `set_status`):
```rust
    /// Compare-and-set: flip `status` from `'pending'` to `'complete'`
    /// atomically. Returns `true` iff this call performed the transition
    /// (`rows_affected == 1`). The single fire-gate for `AttachmentReceived`,
    /// so the direct (3.B) and offline (3.C) lanes cannot both emit on a
    /// simultaneous completion.
    pub fn set_status_if_pending(&self, attachment_id: &[u8; 16]) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "UPDATE attachments SET status = 'complete' \
                     WHERE attachment_id = ?1 AND status = 'pending'",
                    rusqlite::params![&attachment_id[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "attachments set_status_if_pending: {e}"
                    )))
                })?;
            Ok(n == 1)
        })
    }
```

- [ ] **Step 4: Run repo test, verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core attachments::tests::set_status_if_pending 2>&1 | tail -10`
Expected: PASS (first call `true`, second `false`).

- [ ] **Step 5: Gate the direct-lane emit (`finalize_rx`)**

In `crates/core/src/delivery/peer.rs`, in `finalize_rx`'s `Ok(())` arm, replace:
```rust
        Ok(()) => {
            let repo = crate::storage::attachments::AttachmentRepo::new(pool);
            let _ = repo.set_status(&aid, "complete");
            if let Some(d) = inbound.as_ref() {
                d.attachment_received(
                    peer,
                    aid,
                    &safe_name,
                    &manifest.mime,
                    manifest.total_size,
                    &out_path.to_string_lossy(),
                );
            }
            if let Some(c) = conn.as_mut() {
                let _ = c
                    .send(Frame::AttachmentComplete { attachment_id: aid })
                    .await;
            }
        }
```
with (CAS-gate only the `attachment_received` emit; keep the `AttachmentComplete` ack unconditional):
```rust
        Ok(()) => {
            let repo = crate::storage::attachments::AttachmentRepo::new(pool);
            // Fire-gate: only the lane that flips pending→complete emits.
            if repo.set_status_if_pending(&aid).unwrap_or(false) {
                if let Some(d) = inbound.as_ref() {
                    d.attachment_received(
                        peer,
                        aid,
                        &safe_name,
                        &manifest.mime,
                        manifest.total_size,
                        &out_path.to_string_lossy(),
                    );
                }
            }
            if let Some(c) = conn.as_mut() {
                let _ = c
                    .send(Frame::AttachmentComplete { attachment_id: aid })
                    .await;
            }
        }
```

- [ ] **Step 6: Gate the offline-lane emit (`finalize_offline`)**

In `crates/core/src/daemon/inbound.rs`, in `finalize_offline`'s `Ok(())` arm, replace:
```rust
            Ok(()) => {
                let _ = repo.set_status(attachment_id, "complete");
                let _ = store.remove(attachment_id);
                let _ = self.events_tx.send(Event::AttachmentReceived {
                    contact,
                    attachment_id: crate::daemon::hex::Hex16::from(*attachment_id),
                    filename: safe,
                    mime: manifest.mime.clone(),
                    size: manifest.total_size,
                    path: out_path.to_string_lossy().to_string(),
                });
            }
```
with:
```rust
            Ok(()) => {
                let won = repo.set_status_if_pending(attachment_id).unwrap_or(false);
                let _ = store.remove(attachment_id);
                if won {
                    let _ = self.events_tx.send(Event::AttachmentReceived {
                        contact,
                        attachment_id: crate::daemon::hex::Hex16::from(*attachment_id),
                        filename: safe,
                        mime: manifest.mime.clone(),
                        size: manifest.total_size,
                        path: out_path.to_string_lossy().to_string(),
                    });
                }
            }
```

- [ ] **Step 7: Regression — the Phase-3 guardrails must stay green**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core attachment 2>&1 | tail -25 && cargo test -p skattr-tests 2>&1 | tail -20`
Expected: all attachment unit tests + the `offline_attachment_via_mailbox` / `offline_attachment_cross_session_resume` / `attachment_roundtrip_multichunk_over_loopback` guardrails PASS (single completion still fires exactly one event; the gate is transparent to the non-racing path).

- [ ] **Step 8: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/storage/attachments.rs crates/core/src/delivery/peer.rs crates/core/src/daemon/inbound.rs
git commit -m "fix(4.D): compare-and-set fire-gate so attachment completion emits once"
```

---

## Task 3: Item 1 — Log-redaction bypass

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`map_err` + the `add_contact` log)

**Interfaces:**
- Consumes: `DaemonErrorKind` (already `Debug`, from `daemon/error_kind.rs`).
- Produces: `map_err` no longer logs the raw `CoreError` (`?err`) — only the category (`?kind` for typed, a static string for internal).

- [ ] **Step 1: Write the failing test**

In `crates/core/src/daemon/dispatch.rs`, add to the `#[cfg(test)] mod tests` (add `use tracing_subscriber` test deps if needed — `tracing-subscriber` is already a workspace dep):
```rust
    #[test]
    fn map_err_internal_does_not_log_raw_error_text() {
        use std::sync::{Arc, Mutex};
        use tracing::subscriber;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        impl<'a> MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Buf { self.clone() }
        }

        let buf = Buf::default();
        let sub = tracing_subscriber::fmt().with_writer(buf.clone()).finish();
        let secret = "SECRET-INVITE-base64-keymaterial";
        subscriber::with_default(sub, || {
            // An internal (non-kind) error carrying untrusted text.
            let e = crate::error::CoreError::Identity(secret.to_string());
            let _ = map_err(e);
        });
        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("ipc: internal error"), "category logged");
        assert!(!logged.contains(secret), "raw untrusted error text must NOT be logged");
    }
```
> Confirm `CoreError::Identity(String)` exists (it's used by `identity/key.rs`); if the variant differs, pick any `CoreError` variant whose `kind()` is `None` and that carries a `String`.

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core dispatch::tests::map_err_internal 2>&1 | tail -15`
Expected: FAIL — `logged` contains `secret` (the current `tracing::warn!(?err, …)` renders the full error).

- [ ] **Step 3: Implement category-only logging**

In `crates/core/src/daemon/dispatch.rs`, change `map_err` (lines ~1571-1581):
```rust
pub(crate) fn map_err(err: CoreError) -> IpcError {
    if let Some(kind) = err.kind() {
        tracing::warn!(?kind, "ipc: typed daemon error");
        IpcError::Daemon(kind)
    } else {
        let msg = format!("{err}");
        let truncated: String = msg.chars().take(256).collect();
        tracing::warn!("ipc: internal error");
        IpcError::Internal(truncated)
    }
}
```
(Dropped `?err` from both lines. The typed line keeps `?kind` — `DaemonErrorKind` is a closed category enum that carries no onion/pubkey/untrusted text. `truncated` is still returned to the caller as before; it is just no longer written to the log ring buffer.)

The `add_contact` log at line ~432 (`tracing::warn!(?e, "add_contact: could not build self-card to send")`) logs an `IpcError` whose existing doc-comment already asserts it "carries only DaemonErrorKind … no onion or pubkey." **Leave it as-is** (it is already category-only), but add a one-line confirmation comment is unnecessary — no change.

- [ ] **Step 4: Run test, verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core dispatch::tests::map_err_internal 2>&1 | tail -10`
Expected: PASS — log contains the category, not `secret`.

- [ ] **Step 5: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "fix(4.D): log error category only in ipc map_err (redaction bypass)"
```

---

## Task 4: Item 2 — Zeroizing secret CBOR buffers

**Files:**
- Modify: `crates/core/src/identity/key.rs` (`sign_cbor`, `verify_cbor`)
- Modify: `crates/core/src/mls/key_package.rs` (comment on the best-effort copy)

**Interfaces:**
- Produces: no signature change — `sign_cbor`/`verify_cbor` keep the same public API; only the internal scratch buffer becomes `Zeroizing<Vec<u8>>`.

- [ ] **Step 1: Write the failing test**

In `crates/core/src/identity/key.rs`, add to the `#[cfg(test)] mod tests` a compile-level assertion that the scratch type is zeroizing. Since the buffer is internal, assert behavior is unchanged (round-trip) and document the zeroize via a type alias the test references:
```rust
    #[test]
    fn sign_cbor_roundtrips_and_buffer_is_zeroizing() {
        // Behavioral: sign+verify still round-trips after the buffer change.
        let key = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();
        #[derive(serde::Serialize)]
        struct Body { x: u8 }
        let body = Body { x: 7 };
        let sig = key.sign_cbor(&body).unwrap();
        IdentityKey::verify_cbor(&key.public(), &body, &sig).unwrap();
        // Type-level guard: the scratch buffer is Zeroizing<Vec<u8>>.
        let scratch: zeroize::Zeroizing<Vec<u8>> = zeroize::Zeroizing::new(Vec::new());
        assert!(scratch.is_empty());
    }
```
(The behavioral round-trip is the real regression guard; the type line documents the intended buffer type. There is no portable way to assert memory was wiped — `Zeroizing`'s `Drop` is the guarantee.)

- [ ] **Step 2: Run test, verify it passes pre-change (round-trip already works)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core identity::key::tests::sign_cbor_roundtrips 2>&1 | tail -10`
Expected: PASS already (round-trip works today). This test is a guard that the buffer change in Step 3 doesn't break behavior. (This item is a hardening refactor, not a behavior change, so the test is a regression guard rather than red→green.)

- [ ] **Step 3: Wrap the scratch buffers in `Zeroizing`**

In `crates/core/src/identity/key.rs`, change `sign_cbor`:
```rust
    pub fn sign_cbor<T: serde::Serialize>(&self, body: &T) -> Result<Signature> {
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        ciborium::ser::into_writer(body, &mut *bytes)
            .map_err(|e| CoreError::Identity(format!("sign_cbor: {e}")))?;
        Ok(self.sign(&bytes))
    }
```
and `verify_cbor`:
```rust
    pub fn verify_cbor<T: serde::Serialize>(
        pubkey: &PublicKey,
        body: &T,
        signature: &Signature,
    ) -> Result<()> {
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        ciborium::ser::into_writer(body, &mut *bytes)
            .map_err(|_| CoreError::Identity("verification failed".into()))?;
        Self::verify(pubkey, &bytes, signature)
    }
```
(`&mut *bytes` reaches the inner `Vec` for `into_writer`; `&bytes` derefs to `&[u8]` for `sign`/`verify`. No API change.)

- [ ] **Step 4: Note the best-effort MLS copy**

In `crates/core/src/mls/key_package.rs`, at line ~124-130, the `secret_bytes` is already `Zeroizing::new(seed_guard.to_vec())`, but line ~130 passes `secret_bytes.to_vec()` (a non-zeroizing `Vec`) into OpenMLS `from_raw` by value. Add a comment above that call (no behavior change — OpenMLS owns the buffer and we cannot wrap it):
```rust
    // Best-effort: OpenMLS `from_raw` takes the secret `Vec<u8>` by value and
    // owns it thereafter, so we cannot guarantee its wipe. Our own copies are
    // Zeroizing; this final hand-off is the documented residual.
```

- [ ] **Step 5: Run test + regression**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core identity:: mls::key_package 2>&1 | tail -20`
Expected: the round-trip test PASS; all existing identity + key_package tests PASS.

- [ ] **Step 6: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/identity/key.rs crates/core/src/mls/key_package.rs
git commit -m "fix(4.D): zeroize CBOR scratch buffers in sign_cbor/verify_cbor"
```

---

## Task 5: Item 5 — `getuid` uid fallback

**Files:**
- Modify: `crates/core/src/daemon/ipc/server/unix.rs` (`current_uid`)

**Interfaces:**
- Produces: `current_uid()` returns the real process uid via `libc::getuid()` (infallible) instead of the `/proc`→`$UID`→`0` chain.

- [ ] **Step 1: Write the failing/guard test**

In `crates/core/src/daemon/ipc/server/unix.rs` `#[cfg(test)] mod tests`, add:
```rust
    #[cfg(unix)]
    #[test]
    fn current_uid_matches_libc_getuid() {
        // current_uid must equal the kernel's view of the process uid.
        let expected = unsafe { libc::getuid() };
        assert_eq!(current_uid(), expected);
    }
```
(Before the change, `current_uid()` reads `/proc/self`, which on Linux equals `getuid()` — so this test passes on Linux today but documents the contract; on a non-`/proc` host it would currently fall to `$UID`/`0` and fail. The change makes it correct everywhere.)

- [ ] **Step 2: Run test (Linux: passes; documents contract)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core ipc::server::unix::tests::current_uid_matches 2>&1 | tail -10`
Expected on this Linux box: PASS (current `/proc/self` path already equals `getuid()`). The change in Step 3 makes it correct on non-`/proc` Unix too and removes the dangerous `→ 0` fallback.

- [ ] **Step 3: Replace the fallback chain with `libc::getuid()`**

In `crates/core/src/daemon/ipc/server/unix.rs`, replace `current_uid`:
```rust
/// Return the effective UID of the current process via `libc::getuid()`.
///
/// `getuid()` is an infallible POSIX syscall, so this is a trivially-safe
/// `unsafe` call on a security boundary (the IPC peer-cred check). It replaces
/// the former `/proc/self` → `$UID` → `0` chain, which was fragile on
/// non-`/proc` Unix (macOS) and could spuriously fall back to root (`0`).
#[cfg(unix)]
pub(crate) fn current_uid() -> PeerId {
    unsafe { libc::getuid() }
}
```
(`libc` is already a direct dep of core — `crates/core/Cargo.toml:85`. `PeerId` is `u32`; `libc::getuid()` returns `libc::uid_t` = `u32` on the supported targets — if clippy flags a cast on some target, use `unsafe { libc::getuid() } as PeerId`.)

- [ ] **Step 4: Run test + the peer-cred tests**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core ipc::server::unix 2>&1 | tail -15`
Expected: `current_uid_matches_libc_getuid` PASS and the three `check_peer_uid_*` tests still PASS.

- [ ] **Step 5: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/daemon/ipc/server/unix.rs
git commit -m "fix(4.D): use libc::getuid() for the IPC peer-cred uid (drop /proc->UID->0)"
```

---

## Task 6: P1 — Opener path confinement to downloads dir

**Files:**
- Modify: `crates/ui/src/attachments.rs` (`validate_openable`, `open_file`, `reveal_in_folder` + tests)
- Modify: `crates/ui/src/main.rs` (no change expected — downloads is derivable from `AppState.data_dir`; confirm)

**Interfaces:**
- Consumes: `AppState.data_dir: parking_lot::RwLock<Option<PathBuf>>` (`crates/ui/src/daemon.rs`); downloads dir = `data_dir.join("downloads")`.
- Produces: `validate_openable(path: &str, downloads: &Path) -> Result<PathBuf, String>` (now takes the confinement root); `open_file`/`reveal_in_folder` resolve the downloads dir from `tauri::State<AppState>` and pass it in.

- [ ] **Step 1: Write the failing tests**

In `crates/ui/src/attachments.rs` `#[cfg(test)] mod tests`, replace the three `validate_*` tests with downloads-confined versions and add an "outside" case:
```rust
    #[test]
    fn validate_inside_downloads_ok() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path();
        let p = downloads.join("f.txt");
        std::fs::write(&p, b"hi").unwrap();
        let got = validate_openable(&p.to_string_lossy(), downloads).unwrap();
        assert!(got.is_absolute());
    }

    #[test]
    fn validate_outside_downloads_errs() {
        let downloads = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = other.path().join("f.txt");
        std::fs::write(&p, b"hi").unwrap();
        let err = validate_openable(&p.to_string_lossy(), downloads.path()).unwrap_err();
        assert!(err.contains("outside download dir"));
    }

    #[test]
    fn validate_missing_file_errs() {
        let downloads = tempfile::tempdir().unwrap();
        let err = validate_openable("/no/such/zzz", downloads.path()).unwrap_err();
        assert!(err.contains("not found") || err.contains("canonicalize"));
    }

    #[test]
    fn validate_directory_errs() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_openable(&dir.path().to_string_lossy(), dir.path()).unwrap_err();
        assert!(err.contains("not a regular file"));
    }
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests::validate 2>&1 | tail -15`
Expected: FAIL — `validate_openable` takes one arg (arity mismatch).

- [ ] **Step 3: Confine `validate_openable` + thread downloads into the commands**

In `crates/ui/src/attachments.rs`, change `validate_openable`:
```rust
/// Canonicalize a UI-supplied path, assert it is an existing regular file,
/// AND confine it to the daemon's `downloads` dir. Defense-in-depth:
/// received-file paths are daemon-authored, but this makes open/reveal
/// safe-by-construction rather than safe-by-context.
fn validate_openable(path: &str, downloads: &std::path::Path) -> Result<PathBuf, String> {
    let canon = std::fs::canonicalize(path).map_err(|e| format!("canonicalize {path}: {e}"))?;
    let canon_downloads =
        std::fs::canonicalize(downloads).map_err(|e| format!("canonicalize downloads: {e}"))?;
    if !canon.starts_with(&canon_downloads) {
        return Err(format!("{path}: outside download dir"));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("{path}: not found: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    Ok(canon)
}

/// Resolve `<data_dir>/downloads` from managed state.
fn downloads_dir(state: &tauri::State<'_, crate::daemon::AppState>) -> Result<PathBuf, String> {
    let dd = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    Ok(dd.join("downloads"))
}
```
and change the two commands to take `state` and pass the dir:
```rust
#[tauri::command]
pub async fn open_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::daemon::AppState>,
    path: String,
) -> Result<(), String> {
    let downloads = downloads_dir(&state)?;
    let canon = validate_openable(&path, &downloads)?;
    app.opener()
        .open_path(canon.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("open_file: {e}"))
}

#[tauri::command]
pub async fn reveal_in_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::daemon::AppState>,
    path: String,
) -> Result<(), String> {
    let downloads = downloads_dir(&state)?;
    let canon = validate_openable(&path, &downloads)?;
    app.opener()
        .reveal_item_in_dir(canon)
        .map_err(|e| format!("reveal_in_folder: {e}"))
}
```
(Tauri injects `tauri::State<AppState>` the same way `ipc_request` already receives it — no `generate_handler!` change needed; the command signature change is transparent to the JS caller, which still passes only `{ path }`.)

- [ ] **Step 4: Run tests, verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-ui attachments::tests 2>&1 | tail -15`
Expected: all `validate_*` tests PASS (incl. `validate_outside_downloads_errs`); the existing decode/file_size tests still PASS.

- [ ] **Step 5: Build + clippy (confirms the Tauri command signatures still register)**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-ui 2>&1 | tail -8 && cargo clippy -p skattr-ui --all-targets --all-features -- -D warnings 2>&1 | tail -8`
Expected: clean build (the `state: tauri::State<AppState>` injection compiles against `generate_handler!`).

- [ ] **Step 6: fmt + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check`
```bash
git add crates/ui/src/attachments.rs
git commit -m "fix(4.D): confine open_file/reveal_in_folder to the downloads dir"
```

---

## Task 7: Item 3 — `data_dir` created 0700

**Files:**
- Modify: `crates/core/src/storage/pool.rs` (`Pool::open`)

**Interfaces:**
- Produces: on Unix, `Pool::open` creates/enforces `data_dir` mode `0700`.

- [ ] **Step 1: Write the failing test**

In `crates/core/src/storage/pool.rs` `#[cfg(test)] mod tests`, add:
```rust
    #[cfg(unix)]
    #[test]
    fn open_sets_data_dir_0700() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("skattr-data");
        let seed = crate::identity::Seed::generate().unwrap();
        let _pool = Pool::open(&data_dir, &seed).unwrap();
        let mode = std::fs::metadata(&data_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "data_dir must be private (0700)");
    }
```

- [ ] **Step 2: Run test, verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core pool::tests::open_sets_data_dir_0700 2>&1 | tail -12`
Expected: FAIL — mode is the umask default (e.g. `0755`), not `0700`.

- [ ] **Step 3: Create the dir 0700 on Unix**

In `crates/core/src/storage/pool.rs`, in `Pool::open`, replace the first line `std::fs::create_dir_all(data_dir)?;` with:
```rust
        std::fs::create_dir_all(data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Private to the owning user: the dir holds the encrypted DB +
            // sentinels. Enforce even if a prior umask created it 0755.
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
```
(`set_permissions` after `create_dir_all` is robust against an existing dir with looser perms; `#[cfg(unix)]` so Windows is unchanged. `?` propagates `std::io::Error` → `CoreError` via the existing `From` used elsewhere in `open`.)

- [ ] **Step 4: Run test + regression**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core pool:: 2>&1 | tail -15`
Expected: `open_sets_data_dir_0700` PASS; all existing pool tests PASS.

- [ ] **Step 5: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/storage/pool.rs
git commit -m "fix(4.D): create data_dir 0700 (private to the owning user)"
```

---

## Task 8: P3 — `chunk_sweep` warn logs

**Files:**
- Modify: `crates/core/src/delivery/chunk_sweep.rs`

**Interfaces:** none (observability only).

- [ ] **Step 1: Add `warn!` on the swallowed writes**

In `crates/core/src/delivery/chunk_sweep.rs`, surface the dropped write errors. `tracing` is used elsewhere via the full path (`tracing::warn!` already appears in `due()` handling), so no new import is required. Change each `let _ = …` write to log on error:

In `run_chunk_sweep`, the chunk-missing branch (line ~54):
```rust
                if let Err(e) = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index) {
                    tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "mark_deposited (missing-chunk drop) failed");
                }
```
The deposit-success branch (line ~72):
```rust
                    if let Err(e) = deposit_repo.mark_deposited(&row.attachment_id, row.chunk_index) {
                        tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "mark_deposited failed");
                    }
```
The prune block (lines ~89-91):
```rust
            if let Err(e) = deposit_repo.delete_for_attachment(&row.attachment_id) {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "delete_for_attachment failed");
            }
            if let Err(e) = chunk_store.remove(&row.attachment_id) {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "chunk_store remove failed");
            }
            if let Err(e) = AttachmentRepo::new(pool).set_status(&row.attachment_id, "complete") {
                tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "set_status complete failed");
            }
```
And in `reschedule` (line ~104):
```rust
    if let Err(e) = repo.reschedule(&row.attachment_id, row.chunk_index, attempts, next) {
        tracing::warn!(target: "skattr::delivery::chunk_sweep", error = %e, "reschedule failed");
    }
```
(`chunk_store.remove` / `delete_for_attachment` / etc. return `Result`; `%e` uses `Display`. Keep the non-fatal control flow — these stay best-effort, just observable.)

- [ ] **Step 2: Build + the existing chunk_sweep tests stay green**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core chunk_sweep 2>&1 | tail -15`
Expected: existing `chunk_sweep` tests PASS (no behavior change — only logging added). Output pristine.

- [ ] **Step 3: Gate + commit**

Run: `. "$HOME/.cargo/env" && cargo fmt --all --check && cargo clippy -p skattr-core --all-targets --all-features -- -D warnings 2>&1 | tail -8`
```bash
git add crates/core/src/delivery/chunk_sweep.rs
git commit -m "fix(4.D): warn! on swallowed chunk_sweep writes (observability)"
```

---

## Task 9: P5 — `PromotedMessage` cast cleanup

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/conversation.ts`

**Interfaces:**
- Produces: `export type PromotedMessage = Omit<OptimisticMessage, "__optimistic"> & { __optimistic: false }`; `sendFile`'s promotion uses it instead of `as unknown as OptimisticMessage`.

- [ ] **Step 1: Add the `PromotedMessage` type**

In `crates/ui/src-svelte/src/lib/stores/conversation.ts`, after the `OptimisticMessage` type (line ~15), add:
```typescript
/**
 * An optimistic message that has been promoted to non-optimistic after the
 * daemon acknowledged it (FileQueued / message_sent), while still carrying the
 * optimistic display fields until the canonical MessageRecord arrives. Narrows
 * `__optimistic` to `false` so the promotion needs no `as unknown` cast.
 */
export type PromotedMessage = Omit<OptimisticMessage, "__optimistic"> & {
  __optimistic: false;
};
```

- [ ] **Step 2: Use it in `sendFile`'s promotion**

In `sendFile` (the promotion `conversation.update` block, line ~302-307), replace:
```typescript
      const next = [...s.messages];
      next[idx] = { ...(next[idx] as OptimisticMessage), __optimistic: false } as unknown as OptimisticMessage;
      return { ...s, messages: next };
```
with:
```typescript
      const next = [...s.messages];
      const promoted: PromotedMessage = {
        ...(next[idx] as OptimisticMessage),
        __optimistic: false,
      };
      next[idx] = promoted;
      return { ...s, messages: next };
```
(`PromotedMessage` is assignable to the `messages` array element type `MessageRecord | OptimisticMessage` because it extends `MessageRecord`; no `unknown` cast.)

- [ ] **Step 3: Type-check + existing tests**

Run: `cd crates/ui/src-svelte && pnpm check 2>&1 | tail -8 && pnpm test src/lib/components/Composer.test.ts 2>&1 | tail -8`
Expected: no NEW svelte-check errors in `conversation.ts` (the 4 pre-existing settings-page errors are unrelated); the Composer/conversation tests PASS.

- [ ] **Step 4: Build + commit**

Run: `cd crates/ui/src-svelte && pnpm build 2>&1 | tail -3`
```bash
cd /home/myggiz/development/skattr
git add crates/ui/src-svelte/src/lib/stores/conversation.ts
git commit -m "refactor(4.D): PromotedMessage type removes the unsound promotion cast"
```

---

## Task 10: Full gate (verification-before-completion)

**Files:** none (verification only).

- [ ] **Step 1: Rust gate**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all --check && \
cargo clippy -p skattr-core -p skattr-ui --all-targets --all-features -- -D warnings && \
cargo test -p skattr-core -p skattr-ui 2>&1 | tail -30
```
Expected: fmt clean; no clippy warnings; all core + ui Rust tests PASS.

- [ ] **Step 2: Attachment guardrail regression**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests 2>&1 | tail -20`
Expected: the Phase-3 attachment guardrails (`offline_attachment_*`, `attachment_roundtrip_multichunk_over_loopback`) + all integration tests PASS — confirms Task 2's CAS gate and Task 1's migration guard didn't regress delivery.

- [ ] **Step 3: Frontend gate**

Run:
```bash
cd crates/ui/src-svelte && \
pnpm install --frozen-lockfile && pnpm check 2>&1 | tail -6 && pnpm build 2>&1 | tail -3 && pnpm test 2>&1 | tail -8
```
Expected: install clean; only the 4 pre-existing settings-page svelte-check errors (none in 4.D-touched files); build succeeds; full Vitest suite PASS.

- [ ] **Step 4: e2e gate (local)**

Run: `cd crates/ui/src-svelte && pnpm test:e2e 2>&1 | tail -20`
Expected: all e2e specs PASS (4.D touches no UI behavior beyond the transparent opener-state arg + the type cleanup).

- [ ] **Step 5: Branch status + handoff**

Run: `git status && git log --oneline master..HEAD`
Expected: clean tree; the nine Task commits listed on `phase-4d-t3-hardening`. Hand off to the whole-branch review → PR → CodeRabbit babysit → merge (per the completion-review rule). Do NOT merge before the whole-branch review.

---

## Self-Review (completed against the spec)

**Spec coverage:** §4 Items 1–5 → Tasks 3, 4, 7, 1, 5; P1/P2/P3/P5 → Tasks 6, 2, 8, 9. The dropped HS-key finding (§2) has no task (correct). §5 testing → per-task unit tests + Task 10 gates + the explicit guardrail-regression step (Task 2 Step 7, Task 10 Step 2). §6 sequencing → task order is Item 4 (T1), P2 (T2), then security items 1/2/5/P1 (T3–T6), then mechanical 3/P3/P5 (T7–T9), matching the spec's risk-ordering. Review tiers carried into the Global Constraints.

**Placeholder scan:** every code step carries literal code. The two soft references — Task 2 Step 1 "if the file's tests use a different Pool constructor, mirror theirs," and Task 3 Step 1 "if the `CoreError` variant differs, pick any kind()==None variant carrying a String" — are concrete fallbacks naming the exact thing to check, not deferrals; each provides a working default (`Pool::open(dir, &seed)`, `CoreError::Identity`).

**Type consistency:** `set_status_if_pending(&[u8;16]) -> Result<bool>` defined in Task 2 Step 3 and used in Steps 5–6 with the same signature; `SchemaTooNew { found, max_known }` defined in Task 1 Step 1 and matched in Step 2's test; `validate_openable(path, downloads)` arity matches between Task 6 Steps 1 and 3; `PromotedMessage` defined in Task 9 Step 1 and used in Step 2.
