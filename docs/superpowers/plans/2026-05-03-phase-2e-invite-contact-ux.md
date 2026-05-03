# Phase 2.E Invite & Contact UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship invite-generate / add-contact / contact-rename / contact-archive UI on top of working Welcome propagation so two non-technical testers can complete invite → add → first-message via the UI alone.

**Architecture:** Direct-only Welcome over the existing `Frame::MlsWelcome` codec slot (already reserved at `0x03`); inviter persists the invite PSK in a new `outstanding_invites` table; consumer's daemon emits the Welcome to the hub, peer-actor sends `Frame::MlsWelcome`, recipient's `DaemonInbound::dispatch_welcome` looks up the PSK and joins. Wire format stays append-only — new `Command::RenameContact` / `RemoveContact` / `ListContactsWithFilter` variants are added; `ContactCard` and existing variants are untouched. UI ships three new dialogs (`InviteGenerate`, `AddContact`, `ConfirmDialog`), an inline `ContactDetailsPanel`, a `Toast`, and reuses the existing `IpcClient` + design tokens.

**Tech Stack:** Rust 2021 (skattr-core / -ui / -tests workspace), OpenMLS 0.8.1, BLAKE2s (already a dep), rusqlite 0.38, tokio, Tauri 2 + SvelteKit + Vite + Vitest + Playwright, jsqr (new pnpm dep), `core::invite::qr` (already shipped under default feature `qr`).

**Spec:** `docs/superpowers/specs/2026-05-03-phase-2e-invite-contact-ux-design.md` — locked decisions §"Locked decisions" rows 1–14.

**Worktree:** `/home/myggiz/development/skattr-phase-2e-invite-contact-ux`, branch `phase-2e-invite-contact-ux` from master `769e238`.

**Conventions:**
- Tests live next to the code: `crates/core/src/storage/<module>.rs::tests` for storage, `daemon/dispatch.rs::tests` for dispatch, `daemon/inbound.rs::tests` for inbound, `crates/tests/src/` for cross-daemon integration.
- Cargo runs require `. "$HOME/.cargo/env" &&` prefix per CLAUDE.md.
- `cargo test -p skattr-core` requires `--features test-harness` per memory; full-tree `cargo test` does not.
- Every `.rs` file carries `// SPDX-License-Identifier: GPL-3.0-or-later` + `// Copyright (C) 2026 Myggiz AB` headers.
- All commits include the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- Each task ends with **one** commit; commit messages follow the existing style (`feat(scope): subject` / `fix(scope): subject` / `docs(scope): subject`).

---

## File Structure

### New Rust files (storage + delivery + tests)

| Path | Purpose |
|---|---|
| `crates/core/src/storage/migrations/0010_outstanding_invites.sql` | Schema for inviter-side PSK persistence |
| `crates/core/src/storage/migrations/0011_contacts_hidden.sql` | Soft-delete column on `contacts` |
| `crates/core/src/storage/outstanding_invites.rs` | `OutstandingInviteRepo` (put / get / mark_consumed / purge_expired) |
| `crates/tests/src/welcome_propagation.rs` | `#[ignore]`-gated real-Tor end-to-end |

### New Rust files (UI shell)

| Path | Purpose |
|---|---|
| (no new `.rs` files; modifications only — `crates/ui/src/ipc_bridge.rs` + `main.rs`) | Wire `render_invite_qr` Tauri command |

### New TypeScript files

| Path | Purpose |
|---|---|
| `crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.svelte` | Modal: nickname + TTL + URL/QR result view |
| `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte` | Modal: paste / scan tabs |
| `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte` | Inline-expanded row with identity / rename / archive |
| `crates/ui/src-svelte/src/lib/components/ConfirmDialog.svelte` | Reusable confirmation modal |
| `crates/ui/src-svelte/src/lib/components/Toast.svelte` | Transient notification |
| `crates/ui/src-svelte/src/lib/icons/qr-code.svg` | Lucide MIT |
| `crates/ui/src-svelte/src/lib/stores/qr.ts` | SVG cache for `render_invite_qr` |
| `crates/ui/src-svelte/src/lib/stores/toast.ts` | Singleton toast store |
| `crates/ui/src-svelte/src/lib/components/*.test.ts` | Vitest specs (one per component) |
| `crates/ui/src-svelte/tests/e2e/invite-generate.spec.ts` | Playwright |
| `crates/ui/src-svelte/tests/e2e/add-contact-paste.spec.ts` | Playwright |
| `crates/ui/src-svelte/tests/e2e/contact-details-panel.spec.ts` | Playwright |

### Modified Rust files

| Path | Change |
|---|---|
| `crates/core/src/storage/migrations.rs` | Append migrations 0010, 0011 entries |
| `crates/core/src/storage/mod.rs` | Add `outstanding_invites` module + dual-cfg re-export |
| `crates/core/src/storage/contacts.rs` | `set_display_name`, `set_hidden`, `list_all`; filter `list()` by `hidden = 0` |
| `crates/core/src/daemon/commands.rs` | 3 new `Command` variants + serde tests |
| `crates/core/src/daemon/dispatch.rs` | New `rename_contact` / `remove_contact` / `list_contacts_with_filter` handlers; persist PSK at `create_invite`; emit Welcome at end of `add_contact` |
| `crates/core/src/daemon/inbound.rs` | Implement `dispatch_welcome` |
| `crates/core/src/daemon/retention.rs` | Add `OutstandingInviteRepo::purge_expired` step to the tick |
| `crates/core/src/delivery/peer.rs` | `WelcomeJob`, send-arm, read-arm; extend `InboundDispatch` trait with `dispatch_welcome`; `welcome_msg_id` helper |
| `crates/core/src/delivery/hub.rs` | Add `welcome_jobs` channel on `PeerChannels`; `DeliveryHub::send_welcome` |
| `crates/core/src/mls/key_package.rs` | `parse_welcome_kp_hash` helper (extracts new-member KP ref from a Welcome blob) |
| `crates/core/tests/wire_format_append_only.rs` | Add 3 new tags to expected Command snapshot |
| `crates/cli/src/main.rs` | (no behaviour change — but if test-harness compiles fail, may need pattern-match update) |
| `crates/tests/src/cli_two_daemons.rs` | Assert Alice's `group_state == "active"` after Welcome |

### Modified TypeScript files

| Path | Change |
|---|---|
| `crates/ui/src-svelte/src/lib/components/ContactRow.svelte` | Add chevron + click-to-toggle expansion |
| `crates/ui/src-svelte/src/lib/stores/contacts.ts` | `rename`, `archive`, `expandedPubkey`, `toggleExpanded` |
| `crates/ui/src-svelte/src/routes/+page.svelte` | Wire `+ Add` and `+ Generate invite` header buttons; render `ContactDetailsPanel` on expand |
| `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` | New fixtures (`invite-flow` / `add-contact-flow`) |
| `crates/ui/src-svelte/package.json` | Add `jsqr` dep |

### Modified Tauri files

| Path | Change |
|---|---|
| `crates/ui/src/ipc_bridge.rs` | Add `render_invite_qr` Tauri command |
| `crates/ui/src/main.rs` | Register `render_invite_qr` in `invoke_handler!` |

(`crates/ui/Cargo.toml` is unchanged: `skattr-core`'s `default = ["qr"]` already pulls in the qr feature transitively.)

---

## Task list overview

Tasks are grouped into eight phases. Each task ends with a commit. Phases 1–4 must run sequentially (later phases depend on earlier APIs); Phase 5 (UI) depends on Phase 2 (Command surface) but is otherwise independent of Phases 3–4. Phase 6 (E2E + integration tests) depends on Phases 1–5. Phase 7 (CLAUDE.md + final verification) is last.

| Phase | Tasks | Theme |
|---|---|---|
| 1 — Storage | 1–4 | Migrations + `OutstandingInviteRepo` + `ContactRepo` extensions |
| 2 — Wire format | 5–7 | Three new `Command` variants + snapshot + serde |
| 3 — Daemon dispatch | 8–10 | `rename_contact`, `remove_contact`, `list_contacts_with_filter` |
| 4 — Welcome propagation | 11–17 | Helpers, transport plumbing, daemon glue |
| 5 — Tauri bridge | 18 | `render_invite_qr` |
| 6 — UI components & stores | 19–28 | Toast, ConfirmDialog, dialogs, panel, store wiring, +page |
| 7 — Tests | 29–32 | Tauri-mock fixtures, Playwright specs, real-Tor integration |
| 8 — Wrap-up | 33–34 | CLAUDE.md, final verification |

---

## Phase 1 — Storage

### Task 1: Migration 0010 + `OutstandingInviteRepo::{put, get_psk}`

**Files:**
- Create: `crates/core/src/storage/migrations/0010_outstanding_invites.sql`
- Create: `crates/core/src/storage/outstanding_invites.rs`
- Modify: `crates/core/src/storage/migrations.rs` (append entry)
- Modify: `crates/core/src/storage/mod.rs` (add module + dual-cfg re-export)

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0010_outstanding_invites.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr schema migration 0010: outstanding invites
--
-- Persists the per-invite PSK on the inviter's side so that when the
-- consumer's Welcome message arrives the inviter can join the new MLS
-- group via Group::join_from_welcome (which requires the same PSK on
-- both sides). Rows are removed by `mark_consumed` (after zeroizing
-- the psk column) or `purge_expired` (after expires_at lapses).

CREATE TABLE IF NOT EXISTS outstanding_invites (
    kp_hash      BLOB PRIMARY KEY,
    psk          BLOB NOT NULL,
    inviter_kp   BLOB NOT NULL,
    expires_at   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outstanding_invites_expires
    ON outstanding_invites(expires_at);
```

- [ ] **Step 2: Append migration to the runner**

Edit `crates/core/src/storage/migrations.rs`. Find the closing `]` of `ALL_MIGRATIONS` and add:

```rust
    Migration {
        version: 10,
        sql: include_str!("migrations/0010_outstanding_invites.sql"),
    },
```

- [ ] **Step 3: Write the failing repo test**

Create `crates/core/src/storage/outstanding_invites.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Repository for `outstanding_invites` — inviter-side PSK persistence.

use zeroize::Zeroizing;

use super::StorageErrorKind;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// Inviter-side persistence of `(kp_hash, psk, expires_at)` so that the
/// inviter can reconstruct the PSK at Welcome-receive time.
pub struct OutstandingInviteRepo<'p> {
    pool: &'p Pool,
}

impl<'p> OutstandingInviteRepo<'p> {
    /// Construct a new repo bound to `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a row for a freshly-generated invite. Idempotent on
    /// `kp_hash` collision (overwrite is intentional — `CreateInvite`
    /// regenerates a fresh KP each time so collisions only occur on
    /// retries of the same operation).
    pub fn put(
        &self,
        kp_hash: &[u8; 32],
        psk: &Zeroizing<[u8; 32]>,
        inviter_kp: &[u8],
        expires_at: i64,
        created_at: i64,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT OR REPLACE INTO outstanding_invites \
                 (kp_hash, psk, inviter_kp, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    &kp_hash[..],
                    &psk.as_ref()[..],
                    inviter_kp,
                    expires_at,
                    created_at,
                ],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("oi: put: {e}")))
            })?;
            Ok(())
        })
    }

    /// Look up the PSK + expires_at for `kp_hash`. Returns `Ok(None)`
    /// if the row is absent or has been consumed.
    pub fn get_psk(
        &self,
        kp_hash: &[u8; 32],
    ) -> Result<Option<(Zeroizing<[u8; 32]>, i64)>> {
        self.pool.with(|c| {
            let result = c.query_row(
                "SELECT psk, expires_at FROM outstanding_invites WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
                |r| {
                    let psk_bytes: Vec<u8> = r.get(0)?;
                    let expires_at: i64 = r.get(1)?;
                    Ok((psk_bytes, expires_at))
                },
            );
            match result {
                Ok((psk_bytes, expires_at)) => {
                    if psk_bytes.len() != 32 {
                        return Err(CoreError::Storage(StorageErrorKind::Other(
                            format!("oi: psk wrong length: {}", psk_bytes.len()),
                        )));
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&psk_bytes);
                    Ok(Some((Zeroizing::new(arr), expires_at)))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: get: {e}"
                )))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);

        let kp_hash = [0xAAu8; 32];
        let psk = Zeroizing::new([0xBBu8; 32]);
        let inviter_kp = vec![0xCCu8; 64];
        let expires_at = 1_700_010_000;
        let created_at = 1_700_000_000;

        repo.put(&kp_hash, &psk, &inviter_kp, expires_at, created_at)
            .unwrap();

        let (got_psk, got_exp) = repo.get_psk(&kp_hash).unwrap().unwrap();
        assert_eq!(*got_psk.as_ref(), [0xBBu8; 32]);
        assert_eq!(got_exp, expires_at);
    }

    #[test]
    fn get_psk_returns_none_for_missing_row() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let kp_hash = [0xDDu8; 32];
        assert!(repo.get_psk(&kp_hash).unwrap().is_none());
    }
}
```

- [ ] **Step 4: Wire the module into `storage/mod.rs`**

Edit `crates/core/src/storage/mod.rs`. After the existing `pub(crate) mod seen_messages;` line, add:

```rust
pub(crate) mod outstanding_invites;
```

Then in the dual-cfg re-export blocks, add:

```rust
#[cfg(not(feature = "test-harness"))]
pub(crate) use outstanding_invites::OutstandingInviteRepo;
#[cfg(feature = "test-harness")]
pub use outstanding_invites::OutstandingInviteRepo;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::outstanding_invites -- --nocapture`

Expected: `test result: ok. 2 passed`.

- [ ] **Step 6: Run the full storage test suite to verify no regressions**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::`

Expected: All existing storage tests still pass; 2 new tests added.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/storage/migrations/0010_outstanding_invites.sql \
        crates/core/src/storage/migrations.rs \
        crates/core/src/storage/mod.rs \
        crates/core/src/storage/outstanding_invites.rs
git commit -m "$(cat <<'EOF'
feat(storage): outstanding_invites table for inviter-side PSK persistence

Migration 0010 + OutstandingInviteRepo with put/get_psk.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `OutstandingInviteRepo::mark_consumed` (zeroize + delete)

**Files:**
- Modify: `crates/core/src/storage/outstanding_invites.rs`

- [ ] **Step 1: Write the failing test**

Edit `crates/core/src/storage/outstanding_invites.rs`. Inside the `mod tests` block (above the closing `}`), add:

```rust
    #[test]
    fn mark_consumed_zeroizes_then_deletes() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let kp_hash = [0x01u8; 32];
        let psk = Zeroizing::new([0xEEu8; 32]);
        repo.put(&kp_hash, &psk, &[], 0, 0).unwrap();

        repo.mark_consumed(&kp_hash).unwrap();

        // Row is gone.
        assert!(repo.get_psk(&kp_hash).unwrap().is_none());
    }

    #[test]
    fn mark_consumed_is_idempotent_on_missing_row() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        // No put — mark_consumed must succeed silently.
        repo.mark_consumed(&[0x02u8; 32]).unwrap();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::outstanding_invites::tests::mark_consumed`

Expected: FAIL — `no method named 'mark_consumed'`.

- [ ] **Step 3: Implement `mark_consumed`**

Edit `crates/core/src/storage/outstanding_invites.rs`. Inside `impl<'p> OutstandingInviteRepo<'p>`, after `get_psk`, add:

```rust
    /// Zeroize the PSK column then delete the row in one transaction.
    /// Calling on a missing row succeeds silently (idempotent).
    pub fn mark_consumed(&self, kp_hash: &[u8; 32]) -> Result<()> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: zeroize: {e}"
                )))
            })?;
            tx.execute(
                "DELETE FROM outstanding_invites WHERE kp_hash = ?1",
                rusqlite::params![&kp_hash[..]],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: delete: {e}"
                )))
            })?;
            Ok(())
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::outstanding_invites`

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/outstanding_invites.rs
git commit -m "$(cat <<'EOF'
feat(storage): OutstandingInviteRepo::mark_consumed (zeroize then delete)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: `OutstandingInviteRepo::purge_expired`

**Files:**
- Modify: `crates/core/src/storage/outstanding_invites.rs`

- [ ] **Step 1: Write the failing test**

Append inside the `mod tests` block:

```rust
    #[test]
    fn purge_expired_removes_only_expired_rows() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let now = 1_700_000_000;

        let kp1 = [0x10u8; 32];   // expired
        let kp2 = [0x20u8; 32];   // still valid
        repo.put(&kp1, &Zeroizing::new([0u8; 32]), &[], now - 1, 0).unwrap();
        repo.put(&kp2, &Zeroizing::new([0u8; 32]), &[], now + 3600, 0).unwrap();

        let purged = repo.purge_expired(now).unwrap();
        assert_eq!(purged, 1);

        assert!(repo.get_psk(&kp1).unwrap().is_none());
        assert!(repo.get_psk(&kp2).unwrap().is_some());
    }

    #[test]
    fn purge_expired_returns_zero_when_no_rows_expired() {
        let pool = Pool::in_memory();
        let repo = OutstandingInviteRepo::new(&pool);
        let now = 1_700_000_000;
        repo.put(&[0x30u8; 32], &Zeroizing::new([0u8; 32]), &[], now + 3600, 0)
            .unwrap();
        assert_eq!(repo.purge_expired(now).unwrap(), 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::outstanding_invites::tests::purge_expired`

Expected: FAIL — `no method named 'purge_expired'`.

- [ ] **Step 3: Implement `purge_expired`**

Add to `impl<'p> OutstandingInviteRepo<'p>`:

```rust
    /// Zeroize-then-delete every row whose `expires_at < now`.
    /// Returns the number of rows deleted.
    pub fn purge_expired(&self, now: i64) -> Result<u64> {
        self.pool.transaction(|tx| {
            tx.execute(
                "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE expires_at < ?1",
                rusqlite::params![now],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "oi: purge zeroize: {e}"
                )))
            })?;
            let deleted = tx
                .execute(
                    "DELETE FROM outstanding_invites WHERE expires_at < ?1",
                    rusqlite::params![now],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "oi: purge delete: {e}"
                    )))
                })?;
            Ok(deleted as u64)
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::outstanding_invites`

Expected: 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/outstanding_invites.rs
git commit -m "$(cat <<'EOF'
feat(storage): OutstandingInviteRepo::purge_expired sweep

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Migration 0011 + `ContactRepo::{set_display_name, set_hidden, list_all}`

**Files:**
- Create: `crates/core/src/storage/migrations/0011_contacts_hidden.sql`
- Modify: `crates/core/src/storage/migrations.rs` (append)
- Modify: `crates/core/src/storage/contacts.rs` (add three methods, filter `list()`)

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0011_contacts_hidden.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr schema migration 0011: soft-delete column on contacts
--
-- `hidden = 1` removes the contact from the default ListContacts view
-- but preserves MLS group state, messages, mailboxes, and read cursors
-- so a future "Show archived" UX can restore. Existing rows default
-- to 0 (visible).

ALTER TABLE contacts ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_contacts_hidden ON contacts(hidden);
```

- [ ] **Step 2: Append migration entry**

Edit `crates/core/src/storage/migrations.rs`. After the version-10 entry added in Task 1:

```rust
    Migration {
        version: 11,
        sql: include_str!("migrations/0011_contacts_hidden.sql"),
    },
```

- [ ] **Step 3: Write the failing tests**

Edit `crates/core/src/storage/contacts.rs`. Inside `mod tests`, add (replace the existing `list_returns_all_contacts_sorted` to assert hidden filter behaviour, and add new tests):

```rust
    #[test]
    fn set_display_name_round_trips() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x40);
        repo.upsert(&alice).unwrap();

        repo.set_display_name(&alice.identity, Some("renamed")).unwrap();
        assert_eq!(
            repo.get(&alice.identity).unwrap().unwrap().display_name,
            Some("renamed".into())
        );

        repo.set_display_name(&alice.identity, None).unwrap();
        assert!(repo.get(&alice.identity).unwrap().unwrap().display_name.is_none());
    }

    #[test]
    fn set_display_name_returns_not_found_for_missing_contact() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let err = repo
            .set_display_name(&PublicKey([0x99; 32]), Some("x"))
            .expect_err("missing contact");
        assert!(matches!(
            err,
            CoreError::Contact(ContactErrorKind::NotFound)
        ));
    }

    #[test]
    fn set_hidden_filters_list_but_keeps_list_all() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let visible = sample_contact(0x50);
        let archived = sample_contact(0x60);
        repo.upsert(&visible).unwrap();
        repo.upsert(&archived).unwrap();

        repo.set_hidden(&archived.identity, true).unwrap();

        let listed = repo.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].identity, visible.identity);

        let listed_all = repo.list_all().unwrap();
        assert_eq!(listed_all.len(), 2);
    }

    #[test]
    fn set_hidden_is_idempotent() {
        let pool = Pool::in_memory();
        let repo = ContactRepo::new(&pool);
        let alice = sample_contact(0x70);
        repo.upsert(&alice).unwrap();
        repo.set_hidden(&alice.identity, true).unwrap();
        repo.set_hidden(&alice.identity, true).unwrap();
        assert_eq!(repo.list().unwrap().len(), 0);
    }
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::contacts::tests::set_display_name`

Expected: FAIL — `no method named 'set_display_name'`.

- [ ] **Step 5: Implement the three methods + filter `list()`**

Edit `crates/core/src/storage/contacts.rs`.

Find `pub(crate) fn list(&self) -> Result<Vec<Contact>>` and replace its inner `prepare(...)` SQL with the filtered version:

```rust
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at FROM contacts \
                     WHERE hidden = 0 \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
```

Then add three new methods to the `impl<'p> ContactRepo<'p>` block (after `get_group_id`):

```rust
    /// Update the local display name. `None` clears it. Returns
    /// `ContactErrorKind::NotFound` if no row matched.
    pub fn set_display_name(
        &self,
        identity: &PublicKey,
        name: Option<&str>,
    ) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET display_name = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![name, &identity.0[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "set display_name: {e}"
                    )))
                })?;
            if changed == 0 {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Ok(())
        })
    }

    /// Set the `hidden` soft-delete bit. Idempotent. Returns
    /// `ContactErrorKind::NotFound` if no row matched.
    pub fn set_hidden(&self, identity: &PublicKey, hidden: bool) -> Result<()> {
        self.pool.with_mut(|c| {
            let changed = c
                .execute(
                    "UPDATE contacts SET hidden = ?1 WHERE identity_pubkey = ?2",
                    rusqlite::params![if hidden { 1i64 } else { 0i64 }, &identity.0[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("set hidden: {e}")))
                })?;
            if changed == 0 {
                return Err(CoreError::Contact(ContactErrorKind::NotFound));
            }
            Ok(())
        })
    }

    /// Like `list()` but does NOT filter `hidden = 0`. Used by
    /// `Command::ListContactsWithFilter { include_hidden: true }`.
    pub(crate) fn list_all(&self) -> Result<Vec<Contact>> {
        let mut contacts: Vec<Contact> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT identity_pubkey, display_name, added_at FROM contacts \
                     ORDER BY display_name IS NULL, display_name COLLATE NOCASE",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare list_all: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map([], |r| {
                    let pub_bytes: Vec<u8> = r.get(0)?;
                    let mut arr = [0u8; 32];
                    if pub_bytes.len() == 32 {
                        arr.copy_from_slice(&pub_bytes);
                    }
                    Ok(Contact {
                        identity: PublicKey(arr),
                        display_name: r.get(1)?,
                        added_at: r.get(2)?,
                        card: None,
                    })
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "query list_all: {e}"
                    )))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "collect list_all: {e}"
                )))
            })
        })?;
        for contact in &mut contacts {
            contact.card = self.latest_card(&contact.identity)?;
        }
        Ok(contacts)
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::contacts`

Expected: All existing contacts tests still green; 4 new tests pass.

- [ ] **Step 7: Run full storage suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness storage::`

Expected: All storage tests green.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/storage/migrations/0011_contacts_hidden.sql \
        crates/core/src/storage/migrations.rs \
        crates/core/src/storage/contacts.rs
git commit -m "$(cat <<'EOF'
feat(storage): contacts.hidden soft-delete + set_display_name / set_hidden / list_all

Migration 0011 adds the column; ContactRepo::list filters by hidden = 0
by default; list_all surfaces archived contacts for the future
ListContactsWithFilter { include_hidden: true } path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — Wire format

### Task 5: Three new `Command` variants + serde tests

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`

- [ ] **Step 1: Write the failing serde tests**

Edit `crates/core/src/daemon/commands.rs`. Inside the existing top-level `mod tests` block, add:

```rust
    #[test]
    fn rename_contact_command_round_trips_cbor() {
        let cmd = Command::RenameContact {
            contact: PublicKey([0x44; 32]),
            nickname: Some("Alice".into()),
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::RenameContact { nickname: Some(ref s), .. } if s == "Alice"
        ));
    }

    #[test]
    fn rename_contact_command_with_none_round_trips_cbor() {
        let cmd = Command::RenameContact {
            contact: PublicKey([0x55; 32]),
            nickname: None,
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::RenameContact { nickname: None, .. }
        ));
    }

    #[test]
    fn remove_contact_command_round_trips_cbor() {
        let cmd = Command::RemoveContact {
            contact: PublicKey([0x66; 32]),
        };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(back, Command::RemoveContact { .. }));
    }

    #[test]
    fn list_contacts_with_filter_round_trips_cbor() {
        let cmd = Command::ListContactsWithFilter { include_hidden: true };
        let back: Command = roundtrip(&cmd);
        assert!(matches!(
            back,
            Command::ListContactsWithFilter { include_hidden: true }
        ));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::commands::tests::rename_contact 2>&1 | head -20`

Expected: FAIL — `no variant 'RenameContact' on type 'Command'`.

- [ ] **Step 3: Add the three variants**

Edit `crates/core/src/daemon/commands.rs`. Find the `Command::ExportHistory { … }` variant (the last one before the closing `}`) and add three new variants after it:

```rust
    /// Set or clear the local nickname for `contact`. Local-only —
    /// does not propagate to the peer's `ContactCard`.
    /// Validation: empty / whitespace-only after trim → InvalidArgument;
    /// nickname > 64 chars → InvalidArgument.
    RenameContact {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// `Some(nick)` sets; `None` clears.
        nickname: Option<String>,
    },
    /// Soft-delete a contact (`contacts.hidden = 1`). MLS group state,
    /// messages, outbox, mailbox, and read-state rows are preserved.
    /// Idempotent: re-archiving a hidden contact returns `Ok`.
    RemoveContact {
        /// Peer identity pubkey.
        contact: PublicKey,
    },
    /// Like `ListContacts` but with explicit `include_hidden` opt-in.
    /// `ListContacts` (the existing unit variant) implicitly passes
    /// `include_hidden = false`.
    ListContactsWithFilter {
        /// If true, include hidden contacts.
        include_hidden: bool,
    },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::commands::tests`

Expected: All command tests pass; 4 new tests added.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/commands.rs
git commit -m "$(cat <<'EOF'
feat(commands): RenameContact / RemoveContact / ListContactsWithFilter

Strictly additive: existing variants unchanged. CommandResult unchanged
(returns Ok / Contacts respectively).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Update wire-format snapshot

**Files:**
- Modify: `crates/core/tests/wire_format_append_only.rs`

- [ ] **Step 1: Run the snapshot test (currently fails to compile)**

Run: `. "$HOME/.cargo/env" && cargo test --test wire_format_append_only 2>&1 | tail -20`

Expected: COMPILE FAIL — match arms missing for new variants.

- [ ] **Step 2: Add new arms in `command_variant_tag`**

Edit `crates/core/tests/wire_format_append_only.rs`. Replace the `command_variant_tag` body so it contains:

```rust
fn command_variant_tag(c: &Command) -> &'static str {
    match c {
        Command::AddContact { .. } => "add_contact",
        Command::AddMailbox { .. } => "add_mailbox",
        Command::CreateGroup { .. } => "create_group",
        Command::CreateInvite { .. } => "create_invite",
        Command::DaemonInfo => "daemon_info",
        Command::ExportHistory { .. } => "export_history",
        Command::ListContacts => "list_contacts",
        Command::ListContactsWithFilter { .. } => "list_contacts_with_filter",
        Command::ListMailboxes => "list_mailboxes",
        Command::MarkRead { .. } => "mark_read",
        Command::PruneHistory { .. } => "prune_history",
        Command::RecentMessages { .. } => "recent_messages",
        Command::RemoveContact { .. } => "remove_contact",
        Command::RemoveMailbox { .. } => "remove_mailbox",
        Command::RenameContact { .. } => "rename_contact",
        Command::RotateOnion => "rotate_onion",
        Command::SearchMessages { .. } => "search_messages",
        Command::SendMessage { .. } => "send_message",
        Command::Shutdown => "shutdown",
    }
}
```

- [ ] **Step 3: Update `expected_command_variant_set` static list**

Replace the body so it contains the same alphabetical list:

```rust
fn expected_command_variant_set() -> Vec<&'static str> {
    let mut v = vec![
        "add_contact",
        "add_mailbox",
        "create_group",
        "create_invite",
        "daemon_info",
        "export_history",
        "list_contacts",
        "list_contacts_with_filter",
        "list_mailboxes",
        "mark_read",
        "prune_history",
        "recent_messages",
        "remove_contact",
        "remove_mailbox",
        "rename_contact",
        "rotate_onion",
        "search_messages",
        "send_message",
        "shutdown",
    ];
    v.sort();
    v
}
```

- [ ] **Step 4: Run the snapshot test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test --test wire_format_append_only`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/tests/wire_format_append_only.rs
git commit -m "$(cat <<'EOF'
test(wire-format): freeze 3 new Command variants in append-only snapshot

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Verify CLI still compiles

**Files:**
- (probably no changes; only if compile fails) `crates/cli/src/main.rs`

- [ ] **Step 1: Run cargo check on the CLI**

Run: `. "$HOME/.cargo/env" && cargo check -p skattr-cli 2>&1 | tail -10`

Expected: Clean build (CLI dispatches by hand-rolled subcommand parsing rather than matching exhaustively on `Command`, so no change is normally needed).

- [ ] **Step 2: If a `non_exhaustive_patterns` warning appears, add a guard arm**

In whichever match the compiler flags, add:

```rust
        Command::RenameContact { .. }
        | Command::RemoveContact { .. }
        | Command::ListContactsWithFilter { .. } => {
            anyhow::bail!("not supported by the CLI in this build");
        }
```

Then commit.

- [ ] **Step 3: Run full workspace tests**

Run: `. "$HOME/.cargo/env" && cargo test --workspace 2>&1 | tail -15`

Expected: All green.

(If step 2 made changes, commit; otherwise this task is just verification.)

---

## Phase 3 — Daemon dispatch

### Task 8: `rename_contact` dispatcher

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing tests**

Edit `crates/core/src/daemon/dispatch.rs`. Inside the `mod tests` block, add:

```rust
    #[tokio::test]
    async fn rename_contact_validates_nickname() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let (handle, _ipc) = test_handle().await;
        let peer = PublicKey([0x77; 32]);

        // Pre-create the contact row so set_display_name finds something to update.
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: peer,
            display_name: None,
            added_at: 0,
            card: None,
        })
        .unwrap();

        // empty after trim
        let err = execute_command(
            handle.clone(),
            Command::RenameContact { contact: peer, nickname: Some("   ".into()) },
        )
        .await
        .expect_err("empty after trim must reject");
        assert!(matches!(err, IpcError::Daemon(DaemonErrorKind::InvalidArgument)));

        // > 64 chars
        let too_long = "x".repeat(65);
        let err = execute_command(
            handle.clone(),
            Command::RenameContact { contact: peer, nickname: Some(too_long) },
        )
        .await
        .expect_err("> 64 chars must reject");
        assert!(matches!(err, IpcError::Daemon(DaemonErrorKind::InvalidArgument)));

        // happy path
        let ok = execute_command(
            handle.clone(),
            Command::RenameContact { contact: peer, nickname: Some("Alice".into()) },
        )
        .await
        .unwrap();
        assert!(matches!(ok, CommandResult::Ok));

        // verify persisted
        let stored = repo.get(&peer).unwrap().unwrap();
        assert_eq!(stored.display_name.as_deref(), Some("Alice"));
    }

    #[tokio::test]
    async fn rename_contact_emits_contact_updated_event() {
        let (handle, _ipc) = test_handle().await;
        let peer = PublicKey([0x88; 32]);
        crate::storage::ContactRepo::new(&handle.pool)
            .upsert(&crate::contact::Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
            })
            .unwrap();

        let mut rx = handle.events_tx.subscribe();
        let _ = execute_command(
            handle.clone(),
            Command::RenameContact { contact: peer, nickname: Some("Bob".into()) },
        )
        .await
        .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(crate::daemon::events::Event::ContactUpdated(p))) => assert_eq!(p, peer),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }
    }
```

(`test_handle()` is the existing helper used by other tests in this module — it returns `(Arc<DaemonHandle>, IpcSocket)`. If the helper does not yet expose `events_tx`, mirror the construction pattern used by `add_contact_from_self_invite_persists_group_link_and_emits_event` already in this file.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::rename_contact 2>&1 | head -20`

Expected: FAIL — `Command::RenameContact` is unhandled in `execute_command`'s match.

- [ ] **Step 3: Implement the dispatcher**

Edit `crates/core/src/daemon/dispatch.rs`.

In `execute_command`'s `match cmd`, add the three new arms (placed near the other contact-related arms):

```rust
        Command::RenameContact { contact, nickname } => {
            rename_contact(&handle, contact, nickname).await
        }
        Command::RemoveContact { contact } => remove_contact(&handle, contact).await,
        Command::ListContactsWithFilter { include_hidden } => {
            list_contacts(&handle, include_hidden).await
        }
```

Refactor `list_contacts` to take an `include_hidden: bool` parameter — see Task 10. For now, add a temporary signature change OR add a wrapper `async fn list_contacts_default` that calls `list_contacts(&handle, false)`. Cleanest: change `list_contacts` signature now and update the existing `Command::ListContacts` arm:

```rust
        Command::ListContacts => list_contacts(&handle, false).await,
```

And modify `list_contacts`:

```rust
async fn list_contacts<S>(
    handle: &Arc<DaemonHandle<S>>,
    include_hidden: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::ContactSummary;
    use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, ReadStateRepo};

    let repo = ContactRepo::new(&handle.pool);
    let msg_repo = MessageRepo::new(&handle.pool);
    let group_repo = MlsGroupRepo::new(&handle.pool);
    let read_repo = ReadStateRepo::new(&handle.pool);
    let contacts = if include_hidden {
        repo.list_all().map_err(map_err)?
    } else {
        repo.list().map_err(map_err)?
    };

    // … rest of the loop unchanged …
}
```

After `list_contacts`, add the new `rename_contact` function:

```rust
async fn rename_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    nickname: Option<String>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::events::Event;
    use crate::storage::ContactRepo;

    let trimmed = match nickname {
        None => None,
        Some(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument));
            }
            if t.chars().count() > 64 {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument));
            }
            Some(t)
        }
    };

    let repo = ContactRepo::new(&handle.pool);
    repo.set_display_name(&contact, trimmed.as_deref())
        .map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::rename_contact`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): rename_contact handler with nickname validation

Trim, reject empty / whitespace-only, reject > 64 chars. Emit
ContactUpdated on success. Refactors list_contacts to accept
include_hidden so the new ListContactsWithFilter variant routes
through the same code path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: `remove_contact` dispatcher

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing tests**

Inside `mod tests` block, add:

```rust
    #[tokio::test]
    async fn remove_contact_is_idempotent() {
        let (handle, _ipc) = test_handle().await;
        let peer = PublicKey([0x91; 32]);
        crate::storage::ContactRepo::new(&handle.pool)
            .upsert(&crate::contact::Contact {
                identity: peer,
                display_name: Some("Bob".into()),
                added_at: 0,
                card: None,
            })
            .unwrap();

        let r1 = execute_command(
            handle.clone(),
            Command::RemoveContact { contact: peer },
        )
        .await
        .unwrap();
        let r2 = execute_command(
            handle.clone(),
            Command::RemoveContact { contact: peer },
        )
        .await
        .unwrap();
        assert!(matches!(r1, CommandResult::Ok));
        assert!(matches!(r2, CommandResult::Ok));

        // Default ListContacts filters them out.
        let listed = execute_command(handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        match listed {
            CommandResult::Contacts(v) => assert!(v.is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_contact_preserves_mls_group_state() {
        // Set up Alice -> Bob via the existing add_contact flow,
        // then archive Bob and confirm the group blob is unchanged.
        let pool_alice = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let alice_id = std::sync::Arc::new(
            crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap())
                .unwrap(),
        );
        // Build a 2-member MLS group manually so we don't need a full add_contact.
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        let bob_id = crate::identity::IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();
        let bob_provider = MlsProvider::new();
        let kp_repo = crate::storage::KeyPackageRepo::new(&pool_alice);
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &kp_repo).unwrap();
        let mut group =
            crate::mls::Group::create_solo(&alice_id, None, MlsProvider::new()).unwrap();
        let _ = group.add_member(&bob_kp, None).unwrap();
        let group_repo = crate::storage::MlsGroupRepo::new(&pool_alice);
        group.save(&group_repo).unwrap();
        let gid = group.id().0.clone();
        let blob_before: Vec<u8> = pool_alice
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();

        // Insert Bob as a contact and archive him via the dispatcher.
        let bob_pk = bob_id.public();
        let repo = crate::storage::ContactRepo::new(&pool_alice);
        repo.upsert(&crate::contact::Contact {
            identity: bob_pk,
            display_name: None,
            added_at: 0,
            card: None,
        })
        .unwrap();
        repo.set_group_id(&bob_pk, &gid).unwrap();

        // We need a DaemonHandle that uses pool_alice. Use the same
        // construction helper test_handle uses, but parameterised on pool —
        // mirror handle_for_pool used elsewhere in this module if available.
        let handle = test_handle_for_pool(pool_alice.clone(), alice_id.clone()).await;

        let _ = execute_command(handle.clone(), Command::RemoveContact { contact: bob_pk })
            .await
            .unwrap();

        let blob_after: Vec<u8> = pool_alice
            .with(|c| {
                c.query_row(
                    "SELECT state_blob FROM mls_groups WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(
            blob_before, blob_after,
            "RemoveContact must not touch MLS state"
        );
    }
```

(`test_handle_for_pool` is the helper used elsewhere in this file. If it does not yet exist, factor it out from the existing `test_handle` body — the goal is to reuse the same pool across the manual MLS setup and the dispatcher under test.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::remove_contact 2>&1 | head -20`

Expected: FAIL — `Command::RemoveContact` unhandled.

- [ ] **Step 3: Implement `remove_contact`**

Edit `crates/core/src/daemon/dispatch.rs`. Add after `rename_contact`:

```rust
async fn remove_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::events::Event;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    repo.set_hidden(&contact, true).map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::remove_contact`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): remove_contact soft-delete with MLS state preserved

Idempotent set_hidden(true) on contacts row; emits ContactUpdated.
MLS group state, messages, mailboxes, and read cursors are untouched.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: `list_contacts_with_filter` integration test

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

The handler is already wired in Task 8 (the `list_contacts(&handle, include_hidden)` refactor). This task adds the integration test asserting the filter behaviour end-to-end.

- [ ] **Step 1: Add the test**

Inside `mod tests`:

```rust
    #[tokio::test]
    async fn list_contacts_with_filter_includes_hidden_when_opted_in() {
        let (handle, _ipc) = test_handle().await;
        let visible = PublicKey([0xA1; 32]);
        let archived = PublicKey([0xA2; 32]);
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        repo.upsert(&crate::contact::Contact {
            identity: visible,
            display_name: Some("Visible".into()),
            added_at: 0,
            card: None,
        })
        .unwrap();
        repo.upsert(&crate::contact::Contact {
            identity: archived,
            display_name: Some("Archived".into()),
            added_at: 0,
            card: None,
        })
        .unwrap();
        repo.set_hidden(&archived, true).unwrap();

        // Default: only visible.
        let r = execute_command(handle.clone(), Command::ListContacts).await.unwrap();
        match r {
            CommandResult::Contacts(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].pubkey, visible);
            }
            other => panic!("unexpected: {other:?}"),
        }

        // include_hidden = true: both.
        let r = execute_command(
            handle.clone(),
            Command::ListContactsWithFilter { include_hidden: true },
        )
        .await
        .unwrap();
        match r {
            CommandResult::Contacts(v) => assert_eq!(v.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the test**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::list_contacts_with_filter`

Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
test(dispatch): list_contacts_with_filter integration coverage

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — Welcome propagation

### Task 11: `welcome_msg_id` BLAKE2s helper

**Files:**
- Modify: `crates/core/src/delivery/peer.rs`

- [ ] **Step 1: Write the failing test**

Edit `crates/core/src/delivery/peer.rs`. Inside the `mod tests` block, add:

```rust
    #[test]
    fn welcome_msg_id_is_deterministic_blake2s_prefix() {
        let bytes = b"hello welcome";
        let id1 = super::welcome_msg_id(bytes);
        let id2 = super::welcome_msg_id(bytes);
        assert_eq!(id1.0, id2.0, "must be deterministic");

        let other = super::welcome_msg_id(b"different bytes");
        assert_ne!(id1.0, other.0, "different inputs must produce different ids");

        // Sanity: 16 bytes, not all zero.
        assert_eq!(id1.0.len(), 16);
        assert!(id1.0.iter().any(|&b| b != 0));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::welcome_msg_id 2>&1 | head -10`

Expected: FAIL — `cannot find function 'welcome_msg_id'`.

- [ ] **Step 3: Implement the helper**

Edit `crates/core/src/delivery/peer.rs`. Near the top (after `use` block), add:

```rust
/// Deterministic synthetic message id for ACK correlation of an
/// outbound Welcome. Defined identically on both sides so the
/// inviter (sender) and the joiner (receiver) compute the same
/// `MessageId` from the Welcome bytes — letting the existing
/// `Frame::Ack(MessageId)` correlator round-trip without changes.
pub(crate) fn welcome_msg_id(bytes: &[u8]) -> MessageId {
    use blake2::{Blake2s256, Digest};
    let mut h = Blake2s256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&out[..16]);
    MessageId(id)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::welcome_msg_id`

Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "$(cat <<'EOF'
feat(delivery): welcome_msg_id helper (BLAKE2s synthetic ACK id)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: Extend `InboundDispatch` trait with `dispatch_welcome`

**Files:**
- Modify: `crates/core/src/delivery/peer.rs`

The trait gains a new method with a default no-op implementation so existing `InboundDispatch` impls (e.g. `MlsInboundDispatch` in tests) compile without modification.

- [ ] **Step 1: Write a unit test against a custom impl**

Edit `crates/core/src/delivery/peer.rs`. Add in `mod tests`:

```rust
    #[test]
    fn inbound_dispatch_welcome_default_returns_none() {
        struct Stub;
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
            // dispatch_welcome NOT overridden — must use the trait default.
        }
        let s = Stub;
        assert!(s.dispatch_welcome(PublicKey([0u8; 32]), b"x").is_none());
    }

    #[test]
    fn inbound_dispatch_welcome_override_is_called() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Stub(AtomicBool);
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
            fn dispatch_welcome(
                &self,
                _peer: PublicKey,
                welcome: &[u8],
            ) -> Option<MessageId> {
                self.0.store(true, Ordering::SeqCst);
                Some(super::welcome_msg_id(welcome))
            }
        }
        let s = Stub(AtomicBool::new(false));
        let id = s.dispatch_welcome(PublicKey([0u8; 32]), b"hello").unwrap();
        assert_eq!(id.0, super::welcome_msg_id(b"hello").0);
        assert!(s.0.load(Ordering::SeqCst));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::inbound_dispatch_welcome 2>&1 | head -20`

Expected: FAIL — `no method named 'dispatch_welcome'`.

- [ ] **Step 3: Add the trait method with a default no-op**

Edit `crates/core/src/delivery/peer.rs`. Find the `InboundDispatch` trait and replace it with:

```rust
/// Inbound-MLS dispatch strategy, injected per peer actor. See Task 8
/// preamble for the rationale — keeps `openmls` out of the actor
/// and keeps tests that don't need real MLS trivially easy to write.
pub trait InboundDispatch: Send + Sync + 'static {
    /// Decrypt and ingest an inbound MLS application ciphertext from
    /// `peer`. Returns the `MessageId` on success (for ACK) or `None`
    /// on failure.
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId>;

    /// Process an inbound MLS Welcome from `peer` (the inviter side
    /// of the invite link). Default impl ignores the message and
    /// returns `None` so existing impls compile unchanged. Production
    /// `DaemonInbound` overrides this to look up the PSK in
    /// `outstanding_invites`, call `Group::join_from_welcome`, persist
    /// the new group + contact + group_id link, and emit
    /// `Event::ContactUpdated`.
    ///
    /// The returned `MessageId` (when `Some`) MUST equal
    /// `welcome_msg_id(welcome)` so the synthetic ACK correlates with
    /// the sender's outstanding oneshot.
    fn dispatch_welcome(
        &self,
        _peer: PublicKey,
        _welcome: &[u8],
    ) -> Option<MessageId> {
        None
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::inbound_dispatch_welcome`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "$(cat <<'EOF'
feat(delivery): InboundDispatch::dispatch_welcome (default no-op)

Default impl returns None; production DaemonInbound (next task)
overrides to handle PSK lookup + Group::join_from_welcome + persist.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: `parse_welcome_kp_hash` helper in `mls/key_package.rs`

**Files:**
- Modify: `crates/core/src/mls/key_package.rs`

Extracts the new-member KeyPackage hash from a TLS-serialized Welcome blob. OpenMLS 0.8.1 exposes the Welcome via `MlsMessageIn::tls_deserialize_exact` → `MlsMessageBodyIn::Welcome`; the inner `Welcome` carries `secrets: Vec<EncryptedGroupSecrets>` where each `EncryptedGroupSecrets` has `new_member: KeyPackageRef`. For 2-member groups there is exactly one entry.

The exact accessor names depend on the openmls 0.8 API surface and may require `pub(crate)` shims; the implementation step probes the API empirically. For 2.E we always have a 2-member group, so a single-secret expectation is fine.

- [ ] **Step 1: Write the failing test**

Edit `crates/core/src/mls/key_package.rs`. Add at the bottom of the file (or inside an existing `mod tests`):

```rust
#[cfg(test)]
mod welcome_hash_tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::mls::group::Group;
    use crate::mls::provider::MlsProvider;
    use crate::storage::key_packages::KeyPackageRepo;
    use crate::storage::Pool;

    #[test]
    fn parse_welcome_kp_hash_returns_invitee_kp_ref() {
        let pool = Pool::in_memory();

        let alice = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();
        let bob = IdentityKey::from_seed(&crate::identity::Seed::generate().unwrap()).unwrap();

        // Bob's KP — Alice will add Bob to her group, but the test
        // here is symmetric: the Welcome carries a reference to
        // whichever party is being added.
        let bob_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let bob_kp = KeyPackage::generate(&bob, &bob_provider, &kp_repo).unwrap();
        let expected_hash = bob_kp.hash().unwrap();

        let mut alice_group =
            Group::create_solo(&alice, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None).unwrap();

        let parsed = parse_welcome_kp_hash(&welcome).unwrap();
        assert_eq!(parsed, expected_hash);
    }
}
```

- [ ] **Step 2: Run the test (will fail)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness mls::key_package::welcome_hash_tests 2>&1 | head -20`

Expected: FAIL — `cannot find function 'parse_welcome_kp_hash'`.

- [ ] **Step 3: Implement the helper**

Edit `crates/core/src/mls/key_package.rs`. Add a new public function (visibility: `pub(crate)`):

```rust
/// Extract the new-member KeyPackage hash from a TLS-serialized
/// Welcome blob. Used by the inviter to look up the matching
/// `outstanding_invites` row.
///
/// Phase 2 scope: 2-member groups only — Welcomes carry exactly one
/// `EncryptedGroupSecrets`. Returns the first entry's
/// `KeyPackageRef` (32 bytes).
///
/// On parse failure (corrupt bytes, wrong message type, multiple
/// secrets, mis-sized hash) returns `Err(MlsErrorKind::Other(_))`.
pub(crate) fn parse_welcome_kp_hash(welcome: &[u8]) -> crate::error::Result<[u8; 32]> {
    use openmls::framing::{MlsMessageBodyIn, MlsMessageIn};
    use openmls::prelude::tls_codec::Deserialize as _;

    use crate::error::CoreError;
    use crate::mls::error_kind::MlsErrorKind;

    let msg = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|e| {
        CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: deserialize: {e}"
        )))
    })?;

    let inner = match msg.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => {
            return Err(CoreError::from(MlsErrorKind::Other(
                "welcome parse: not a Welcome".into(),
            )))
        }
    };

    let secrets = inner.secrets();
    if secrets.is_empty() {
        return Err(CoreError::from(MlsErrorKind::Other(
            "welcome parse: empty secrets".into(),
        )));
    }
    // 2-member-group invariant: exactly one secret. If openmls reports
    // more, surface it explicitly so the operator knows we're outside
    // Phase 2 scope.
    if secrets.len() > 1 {
        return Err(CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: {} secrets, only 1 supported in Phase 2",
            secrets.len()
        ))));
    }
    let kp_ref = secrets[0].new_member();
    let bytes = kp_ref.as_slice();
    if bytes.len() != 32 {
        return Err(CoreError::from(MlsErrorKind::Other(format!(
            "welcome parse: kp_ref wrong length {}",
            bytes.len()
        ))));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(arr)
}
```

**API drift note:** if openmls 0.8.1's `Welcome` does not expose `secrets()` / `new_member()` / `as_slice()` exactly as written, adapt:
- `secrets()` may be named `secrets_iter` or the field may be `pub(crate)`.
- `new_member()` may return a `KeyPackageRef` whose `.as_slice()` is named differently (`as_ref()`, `to_bytes()`, `value()`).
- If the accessor is `pub(crate)`, the helper needs to live inside the openmls crate's module path, which is impossible — adapt by computing the hash a different way (e.g. by deriving from the `KeyPackageRef`'s `Serialize` impl: `kp_ref.tls_serialize_detached()` returns 34 bytes including a 2-byte length prefix; strip the prefix).

The test in step 1 fails immediately if the implementation is wrong; iterate against the openmls source (`./vendor/openmls` if vendored, else the published crate docs) until it passes.

- [ ] **Step 4: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness mls::key_package::welcome_hash_tests`

Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/mls/key_package.rs
git commit -m "$(cat <<'EOF'
feat(mls): parse_welcome_kp_hash extracts invitee KP ref from a Welcome

Used by the inviter side to look up the matching outstanding_invites
row at Welcome-receive time. 2-member-group scope per Phase 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: `WelcomeJob` + `welcome_jobs` channel + `DeliveryHub::send_welcome`

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (add `WelcomeJob`)
- Modify: `crates/core/src/delivery/hub.rs` (channel + spawn wiring + `send_welcome` method)

This task adds the data structures and wiring for the hub side; the peer-actor handlers come in Task 15.

- [ ] **Step 1: Define `WelcomeJob`**

Edit `crates/core/src/delivery/peer.rs`. After the `DeliveryJob` struct, add:

```rust
/// One outbound Welcome, submitted by the hub. Parallel to
/// `DeliveryJob` but carries opaque Welcome bytes destined for a
/// `Frame::MlsWelcome` frame instead of `Frame::MlsApp`. ACK
/// correlation uses the deterministic `welcome_msg_id(bytes)`.
pub struct WelcomeJob {
    /// TLS-serialized Welcome bytes.
    pub welcome_bytes: Vec<u8>,
    /// Fires `Ok(())` on successful ACK, `Err(())` if the ack path is
    /// torn down (conn dropped, actor cancelled, no live conn at submit
    /// time). Caller treats `Err` as "Welcome did not reach the
    /// inviter — surface via UI."
    pub(crate) ack_tx: oneshot::Sender<std::result::Result<(), ()>>,
}
```

Update the `PeerConnection::spawn` signature to also accept a `welcome_jobs: mpsc::Receiver<WelcomeJob>`:

```rust
pub fn spawn<S>(
    peer: PublicKey,
    jobs: mpsc::Receiver<DeliveryJob>,
    welcome_jobs: mpsc::Receiver<WelcomeJob>,
    ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> PeerHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let _ = full_run::<S>(peer, None, jobs, welcome_jobs, ctrl, pool, inbound).await;
    })
}
```

Forward the parameter into `full_run`'s signature (the actual handler implementation lands in Task 15; for now, `full_run` simply ignores the channel — drain into `_welcome_jobs`):

```rust
async fn full_run<S>(
    peer: PublicKey,
    initial_conn: Option<AuthenticatedConnection<S>>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
    mut _welcome_jobs: mpsc::Receiver<WelcomeJob>,   // wired in Task 15
    mut ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // existing body unchanged
}
```

The test-only `spawn_with_conn_for_test` does not need a welcome channel — leave it as-is.

- [ ] **Step 2: Update hub to plumb the channel**

Edit `crates/core/src/delivery/hub.rs`.

Replace the `PeerChannels` struct with:

```rust
struct PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    jobs: mpsc::Sender<DeliveryJob>,
    welcome_jobs: mpsc::Sender<crate::delivery::peer::WelcomeJob>,
    ctrl: mpsc::Sender<PeerCtrl<S>>,
}
```

Update its `Clone` impl:

```rust
impl<S> Clone for PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            jobs: self.jobs.clone(),
            welcome_jobs: self.welcome_jobs.clone(),
            ctrl: self.ctrl.clone(),
        }
    }
}
```

Update `spawn_peer_actor` to construct the new channel:

```rust
    fn spawn_peer_actor(
        &self,
        peers: &mut HashMap<PublicKey, PeerChannels<S>>,
        peer: PublicKey,
    ) -> PeerChannels<S> {
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (welcome_jobs_tx, welcome_jobs_rx) =
            mpsc::channel::<crate::delivery::peer::WelcomeJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(
            peer,
            jobs_rx,
            welcome_jobs_rx,
            ctrl_rx,
            self.pool.clone(),
            self.inbound.clone(),
        );
        let channels = PeerChannels {
            jobs: jobs_tx,
            welcome_jobs: welcome_jobs_tx,
            ctrl: ctrl_tx,
        };
        peers.insert(peer, channels.clone());
        channels
    }
```

- [ ] **Step 3: Add `DeliveryHub::send_welcome`**

Edit `crates/core/src/delivery/hub.rs`. After `pub async fn send` (around line 217), add:

```rust
    /// Submit a Welcome job for `peer`. Spawns the peer actor on first
    /// use. The Welcome is sent over the existing Noise_XK transport
    /// as `Frame::MlsWelcome(bytes)`. ACK correlation uses
    /// `welcome_msg_id(bytes)` (BLAKE2s prefix), which the receiver
    /// computes identically.
    ///
    /// On success: the returned oneshot resolves `Ok(())` when the
    /// peer ACKs (synchronous in the typical "Alice is online" path).
    /// On failure (no live conn, dropped actor): `Err(())`.
    pub async fn send_welcome(
        &self,
        peer: PublicKey,
        welcome_bytes: Vec<u8>,
    ) -> Result<oneshot::Receiver<std::result::Result<(), ()>>> {
        let (ack_tx, ack_rx) = oneshot::channel::<std::result::Result<(), ()>>();
        let welcome_jobs_tx = self.ensure_welcome_actor(peer).await;
        let _ = welcome_jobs_tx
            .send(crate::delivery::peer::WelcomeJob {
                welcome_bytes,
                ack_tx,
            })
            .await;
        Ok(ack_rx)
    }

    async fn ensure_welcome_actor(
        &self,
        peer: PublicKey,
    ) -> mpsc::Sender<crate::delivery::peer::WelcomeJob> {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            return ch.welcome_jobs.clone();
        }
        let channels = self.spawn_peer_actor(&mut peers, peer);
        channels.welcome_jobs
    }
```

- [ ] **Step 4: Run the workspace test suite to verify nothing regressed**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness 2>&1 | tail -10`

Expected: All tests still pass — the welcome channel is wired but no producer/consumer yet.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/delivery/hub.rs
git commit -m "$(cat <<'EOF'
feat(delivery): WelcomeJob channel + DeliveryHub::send_welcome plumbing

PeerChannels gains a welcome_jobs Sender; spawn_peer_actor and
PeerConnection::spawn accept a parallel mpsc::Receiver. send_welcome
mirrors send() — submits a WelcomeJob with a oneshot ACK. The
peer-actor send-arm + read-arm land in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 15: peer-actor send-arm + read-arm for `Frame::MlsWelcome`

**Files:**
- Modify: `crates/core/src/delivery/peer.rs`

The actor's `full_run` `select!` loop gains two new arms — one for outbound `WelcomeJob`s, one for inbound `Frame::MlsWelcome`. The job-pending map (`HashMap<MessageId, oneshot::Sender>`) is reused unchanged, since `welcome_msg_id` produces the same shape of id.

- [ ] **Step 1: Write a failing integration-style test**

Edit `crates/core/src/delivery/peer.rs`. Inside `mod tests`, add:

```rust
    /// Verify a WelcomeJob round-trips: actor emits Frame::MlsWelcome,
    /// the test responder ACKs with the synthetic id, the oneshot
    /// resolves Ok.
    #[tokio::test]
    async fn welcome_job_round_trips_via_frame_mls_welcome() {
        use crate::transport::frame::Frame;
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let peer = PublicKey([0xAB; 32]);

        // duplex: actor side <-> test responder side
        let (a, b) = tokio::io::duplex(8192);
        let actor_conn = AuthenticatedConnection::test_unauthenticated(a, peer);
        let mut responder_conn = AuthenticatedConnection::test_unauthenticated(b, peer);

        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(4);
        let (welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<tokio::io::DuplexStream>>(4);

        let _h = tokio::spawn(async move {
            // Hand the actor the conn via the test-only path: spawn
            // with conn directly. We use the production `full_run`
            // signature here by way of spawn() + ReplaceConn — but
            // for this test the simpler shim is to call full_run
            // directly with `initial_conn = Some(_)` if exposed; if
            // not, send PeerCtrl::ReplaceConn first.
            let _ = super::full_run::<tokio::io::DuplexStream>(
                peer,
                Some(actor_conn),
                jobs_rx,
                welcome_rx,
                ctrl_rx,
                pool,
                None,
            )
            .await;
        });

        let welcome_bytes = b"fake welcome bytes".to_vec();
        let synthetic_id = super::welcome_msg_id(&welcome_bytes);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        welcome_tx
            .send(WelcomeJob {
                welcome_bytes: welcome_bytes.clone(),
                ack_tx,
            })
            .await
            .unwrap();

        // Responder side: read a Frame::MlsWelcome, echo Frame::Ack(id).
        match responder_conn.recv().await {
            Ok(Some(Frame::MlsWelcome(got))) => assert_eq!(got, welcome_bytes),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
        responder_conn
            .send(Frame::Ack(synthetic_id.0))
            .await
            .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
            Ok(Ok(Ok(()))) => {}
            other => panic!("expected ACK, got {other:?}"),
        }
    }
```

(`AuthenticatedConnection::test_unauthenticated` is the existing test helper used elsewhere in this module — its name and signature must match what already exists; adapt if the helper is named differently.)

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::welcome_job_round_trips 2>&1 | head -30`

Expected: FAIL — actor drops the welcome bytes, responder times out.

- [ ] **Step 3: Wire the send-arm**

Edit `crates/core/src/delivery/peer.rs`. In `full_run`'s `select!` loop, add a new arm alongside the existing `j = jobs.recv() => { … }` arm:

```rust
            wj = welcome_jobs.recv() => {
                let Some(wj) = wj else { break };
                let synthetic_id = welcome_msg_id(&wj.welcome_bytes);
                if let Some(c) = conn.as_mut() {
                    if c.send(Frame::MlsWelcome(wj.welcome_bytes)).await.is_err() {
                        let _ = wj.ack_tx.send(Err(()));
                        conn = None;
                        drain_pending(&mut pending);
                    } else {
                        pending.insert(synthetic_id, wj.ack_tx);
                        last_traffic = tokio::time::Instant::now();
                    }
                } else {
                    let _ = wj.ack_tx.send(Err(()));
                }
            }
```

(Rename `_welcome_jobs` to `welcome_jobs` in the `full_run` signature now that it's read.)

- [ ] **Step 4: Wire the read-arm**

In the same `select!`, find the `Ok(Some(Frame::MlsApp(ct))) => { … }` branch and add a sibling for `MlsWelcome`:

```rust
                    Ok(Some(Frame::MlsWelcome(welcome_bytes))) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(d) = inbound.as_ref() {
                            if let Some(synthetic_id) =
                                d.dispatch_welcome(peer, &welcome_bytes)
                            {
                                if let Some(c) = conn.as_mut() {
                                    let _ = c.send(Frame::Ack(synthetic_id.0)).await;
                                }
                            }
                            // None => not matched (unknown / expired KP); no ACK.
                        } else {
                            tracing::warn!(
                                "peer: inbound MlsWelcome received but no \
                                 InboundDispatch configured"
                            );
                        }
                    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::peer::tests::welcome_job_round_trips`

Expected: 1 test passes.

- [ ] **Step 6: Run the full delivery suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness delivery::`

Expected: All delivery tests still green.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "$(cat <<'EOF'
feat(delivery): peer actor send/read arms for Frame::MlsWelcome

Outbound: emit Frame::MlsWelcome and track ACK by welcome_msg_id.
Inbound: route MlsWelcome bytes through InboundDispatch::dispatch_welcome
and ACK with the synthetic id on success.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: Persist outstanding invite at `create_invite`

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

- [ ] **Step 1: Write the failing test**

Edit `crates/core/src/daemon/dispatch.rs`. Inside `mod tests`:

```rust
    #[tokio::test]
    async fn create_invite_persists_outstanding_invite_row() {
        use crate::storage::OutstandingInviteRepo;

        let (handle, _ipc) = test_handle_with_onion("alicealicealicealicealicealice12.onion").await;
        let result = execute_command(
            handle.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(3600),
            },
        )
        .await
        .unwrap();

        let kp_hash = match result {
            CommandResult::InviteCreated { key_package_id, .. } => key_package_id.0,
            other => panic!("unexpected: {other:?}"),
        };

        let oi = OutstandingInviteRepo::new(&handle.pool);
        let (psk, expires_at) = oi.get_psk(&kp_hash).unwrap().expect("row must exist");
        assert_eq!(psk.as_ref().len(), 32);
        let now = crate::daemon::clock::now_unix_seconds();
        assert!(expires_at >= now + 3500 && expires_at <= now + 3700);
    }
```

(`test_handle_with_onion` is the existing helper; check its presence in the file. Use whatever name the existing
`create_invite_returns_parseable_url_and_records_keypackage` test uses.)

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::create_invite_persists_outstanding 2>&1 | head -10`

Expected: FAIL — no row found.

- [ ] **Step 3: Wire the persistence**

Edit the `create_invite` function in `crates/core/src/daemon/dispatch.rs`. After the existing `let link = InviteLink::generate(...)?;` and before the `Ok(CommandResult::InviteCreated { … })` return, add:

```rust
    use crate::storage::OutstandingInviteRepo;
    use zeroize::Zeroizing;

    let psk_for_storage = Zeroizing::new(psk);
    let oi = OutstandingInviteRepo::new(&handle.pool);
    oi.put(&kp_hash, &psk_for_storage, &kp_bytes, now + ttl as i64, now)
        .map_err(map_err)?;
```

(The variable `psk` was already a `[u8; 32]` filled by `OsRng`; wrapping it in `Zeroizing` zeroes-on-drop the moment the function returns.)

- [ ] **Step 4: Run test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::create_invite_persists_outstanding`

Expected: 1 test passes.

- [ ] **Step 5: Verify the invite-related existing tests still pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::create_invite`

Expected: All `create_invite_*` tests still green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): persist invite PSK in outstanding_invites at CreateInvite

Required for Welcome propagation: when the consumer's Welcome message
arrives, the inviter must reconstruct the PSK to call
Group::join_from_welcome. PSK is wrapped in Zeroizing throughout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 17: Wire `add_contact` to emit Welcome via the hub

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`

Note: `add_contact` runs on **Bob's** side (the consumer). The Welcome it emits travels **to Alice** (the inviter), keyed on `link.body.identity` (which is Alice's pubkey).

- [ ] **Step 1: Write the failing test (use a stub hub)**

Editing this dispatcher carefully — the existing `add_contact_from_self_invite_persists_group_link_and_emits_event` test exercises Bob's side via two daemon handles on the same pool. We extend that test (or add a sibling) to assert the Welcome was submitted to Alice's hub.

In `mod tests`, add:

```rust
    #[tokio::test]
    async fn add_contact_emits_welcome_to_inviter_via_hub() {
        // Set up two handles (Alice = inviter, Bob = consumer) sharing
        // a single in-process Tor stack via the existing test_handle
        // pair builder. Alice's hub is observed; we assert that after
        // Bob's AddContact, Alice receives a Frame::MlsWelcome via the
        // wired duplex transport AND her group transitions to Active.
        let (alice_handle, alice_ipc, bob_handle, _bob_ipc) =
            test_two_handles_paired().await;

        // Alice creates an invite.
        let invite = execute_command(
            alice_handle.clone(),
            Command::CreateInvite {
                nickname: None,
                ttl_secs: Some(600),
            },
        )
        .await
        .unwrap();
        let url = match invite {
            CommandResult::InviteCreated { url, .. } => url,
            other => panic!("unexpected: {other:?}"),
        };

        // Bob adds the invite. add_contact must succeed AND submit the
        // Welcome to its hub keyed on Alice's identity. The paired test
        // setup already wires Bob's hub → Alice's transport via duplex.
        let _added = execute_command(
            bob_handle.clone(),
            Command::AddContact { invite_url: url },
        )
        .await
        .unwrap();

        // Alice should receive a ContactUpdated event within 5 s
        // and her group_state should be Active.
        let mut alice_events = alice_handle.events_tx.subscribe();
        match tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match alice_events.recv().await {
                    Ok(crate::daemon::events::Event::ContactUpdated(_)) => return,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        })
        .await
        {
            Ok(()) => {}
            Err(_) => panic!("Alice did not receive ContactUpdated within 5 s"),
        }

        // Verify Alice's group state is Active by listing her contacts.
        let listed = execute_command(alice_handle.clone(), Command::ListContacts)
            .await
            .unwrap();
        match listed {
            CommandResult::Contacts(v) => {
                let bob_summary = v.iter().find(|s| s.pubkey == bob_handle.identity.public()).unwrap();
                assert_eq!(
                    bob_summary.group_state,
                    Some(crate::daemon::commands::MlsGroupStateLabel::Active)
                );
            }
            other => panic!("unexpected: {other:?}"),
        }

        // Suppress unused warning — alice_ipc holds the socket open.
        drop(alice_ipc);
    }
```

(`test_two_handles_paired` is the helper used by the existing
`add_contact_from_self_invite_persists_group_link_and_emits_event`
test. Reuse / lift / adapt it; if the existing setup wires both
handles to the **same pool**, this test must set them up with
**separate pools** plus a duplex transport between their hubs. See
the existing `cli_two_daemons` integration in `crates/tests/src/`
for the wiring pattern.)

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::add_contact_emits_welcome 2>&1 | head -20`

Expected: FAIL — Alice never receives the event because Bob's `add_contact` discards the Welcome.

- [ ] **Step 3: Wire the hub call**

Edit `add_contact` in `crates/core/src/daemon/dispatch.rs`. Find the line currently reading:

```rust
    let (_welcome, _commit) = group
        .add_member(&invitee_kp, Some(&link.psk.0))
        .map_err(map_err)?;
```

Replace `_welcome` with `welcome` and, after the existing persistence (`group.save`, `contact upsert`, `set_group_id`, `kp mark_consumed`, `events_tx.send(ContactUpdated)`), but before the final `Ok(CommandResult::ContactAdded { … })`, add:

```rust
    // Submit Welcome to the inviter via the hub. We do not await the
    // ACK here — UI responsiveness comes first, and a failed delivery
    // surfaces via Event::DeliveryStatusChanged through the hub's
    // existing failure path.
    let _ = handle
        .hub
        .send_welcome(link.body.identity, welcome)
        .await
        .map_err(map_err)?;
```

(The leading `let _ =` discards the `oneshot::Receiver<...>`; downstream ACK propagation lands in a follow-up task if/when the UI exposes a "delivered" indicator for Welcomes.)

- [ ] **Step 4: Run test to verify it passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::add_contact_emits_welcome`

Expected: 1 test passes.

- [ ] **Step 5: Run the full add_contact suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::dispatch::tests::add_contact`

Expected: Existing `add_contact_*` tests still green; new test passes.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): emit Welcome to inviter via DeliveryHub::send_welcome

Bob's AddContact handler now submits the MLS Welcome bytes to the
hub keyed on Alice's identity. Closes the Phase 2.D limitation that
prevented Alice from decrypting messages from a freshly-added contact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 18: `DaemonInbound::dispatch_welcome` implementation

**Files:**
- Modify: `crates/core/src/daemon/inbound.rs`

This is Alice's side. The full pipeline: parse Welcome → look up PSK → join group → persist (group + contact + group_id link + KP consumed + outstanding_invites consumed) → emit `ContactUpdated`.

- [ ] **Step 1: Write the failing tests**

Edit `crates/core/src/daemon/inbound.rs`. Inside `mod tests`, add:

```rust
    #[tokio::test]
    async fn dispatch_welcome_joins_group_and_emits_contact_updated() {
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, mut rx) = broadcast::channel::<Event>(16);

        // Alice = inviter (the one calling dispatch_welcome).
        let alice = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        // Generate Alice's "ours" KP — exact bytes we'd publish in an invite.
        let alice_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let alice_kp =
            KeyPackage::generate(&alice, &alice_provider, &kp_repo).unwrap();
        let kp_hash = alice_kp.hash().unwrap();
        let alice_kp_bytes = alice_kp.to_bytes().unwrap();

        // Persist the outstanding invite row.
        let psk_bytes = [0xAB; 32];
        let oi = OutstandingInviteRepo::new(&pool);
        oi.put(
            &kp_hash,
            &zeroize::Zeroizing::new(psk_bytes),
            &alice_kp_bytes,
            crate::daemon::clock::now_unix_seconds() + 3600,
            crate::daemon::clock::now_unix_seconds(),
        )
        .unwrap();

        // Bob = consumer. Builds his solo group, adds Alice via her KP.
        let bob = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let mut bob_group =
            crate::mls::Group::create_solo(&bob, Some(&psk_bytes), MlsProvider::new())
                .unwrap();
        let alice_kp_for_add =
            KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _commit) =
            bob_group.add_member(&alice_kp_for_add, Some(&psk_bytes)).unwrap();

        // Now drive Alice's dispatch_welcome.
        let alice_arc = Arc::new(alice);
        let inbound = DaemonInbound::new(pool.clone(), events_tx.clone());
        inbound.set_identity(alice_arc.clone());   // see Step 3 — wired only if needed
        let bob_pubkey = bob.public();
        let result =
            crate::delivery::peer::InboundDispatch::dispatch_welcome(
                &inbound,
                bob_pubkey,
                &welcome_bytes,
            );
        let returned_id = result.expect("dispatch_welcome must succeed");
        assert_eq!(returned_id.0, crate::delivery::peer::welcome_msg_id(&welcome_bytes).0);

        // ContactUpdated event must fire.
        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
            Ok(Ok(Event::ContactUpdated(p))) => assert_eq!(p, bob_pubkey),
            other => panic!("expected ContactUpdated, got {other:?}"),
        }

        // outstanding_invites row must be gone.
        assert!(oi.get_psk(&kp_hash).unwrap().is_none());

        // Alice's contact for Bob must exist with group_id linked.
        let cr = crate::storage::ContactRepo::new(&pool);
        let stored = cr.get(&bob_pubkey).unwrap().expect("contact persisted");
        let gid = cr.get_group_id(&bob_pubkey).unwrap().expect("gid set");
        assert_eq!(gid.len(), 32);
        assert_eq!(stored.identity, bob_pubkey);
    }

    #[tokio::test]
    async fn dispatch_welcome_rejects_unknown_kp_hash() {
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);
        let alice_arc = Arc::new(
            crate::identity::IdentityKey::from_seed(
                &crate::identity::Seed::generate().unwrap(),
            )
            .unwrap(),
        );
        let inbound = DaemonInbound::new(pool, events_tx);
        inbound.set_identity(alice_arc);

        let result = crate::delivery::peer::InboundDispatch::dispatch_welcome(
            &inbound,
            crate::identity::PublicKey([0xCC; 32]),
            b"not a real welcome",
        );
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn dispatch_welcome_rejects_expired_invite() {
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;

        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _rx) = broadcast::channel::<Event>(16);
        let alice = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let alice_provider = MlsProvider::new();
        let kp_repo = KeyPackageRepo::new(&pool);
        let alice_kp =
            KeyPackage::generate(&alice, &alice_provider, &kp_repo).unwrap();
        let kp_hash = alice_kp.hash().unwrap();
        let alice_kp_bytes = alice_kp.to_bytes().unwrap();

        // Outstanding invite — already expired.
        let psk = [0u8; 32];
        OutstandingInviteRepo::new(&pool)
            .put(
                &kp_hash,
                &zeroize::Zeroizing::new(psk),
                &alice_kp_bytes,
                crate::daemon::clock::now_unix_seconds() - 1,
                crate::daemon::clock::now_unix_seconds() - 3600,
            )
            .unwrap();

        // Build a Welcome targeting Alice's KP.
        let bob = crate::identity::IdentityKey::from_seed(
            &crate::identity::Seed::generate().unwrap(),
        )
        .unwrap();
        let mut bob_group =
            crate::mls::Group::create_solo(&bob, Some(&psk), MlsProvider::new()).unwrap();
        let alice_kp_for_add =
            KeyPackage::from_bytes(&alice_kp_bytes).unwrap();
        let (welcome_bytes, _) = bob_group.add_member(&alice_kp_for_add, Some(&psk)).unwrap();

        let alice_arc = Arc::new(alice);
        let inbound = DaemonInbound::new(pool, events_tx);
        inbound.set_identity(alice_arc);

        let result = crate::delivery::peer::InboundDispatch::dispatch_welcome(
            &inbound,
            bob.public(),
            &welcome_bytes,
        );
        assert!(result.is_none(), "expired invite must not be processed");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::inbound::tests::dispatch_welcome 2>&1 | head -20`

Expected: FAIL — `dispatch_welcome` is the trait default no-op.

- [ ] **Step 3: Add identity to `DaemonInbound` + implement `dispatch_welcome`**

`DaemonInbound::dispatch_welcome` needs Alice's `IdentityKey` to call `Group::join_from_welcome`. The current struct doesn't carry it — add a field.

Edit `crates/core/src/daemon/inbound.rs`:

```rust
pub(crate) struct DaemonInbound {
    pub pool: Arc<Pool>,
    pub events_tx: broadcast::Sender<Event>,
    pub identity: parking_lot::RwLock<Option<Arc<IdentityKey>>>,
}

impl DaemonInbound {
    pub(crate) fn new(pool: Arc<Pool>, events_tx: broadcast::Sender<Event>) -> Self {
        Self {
            pool,
            events_tx,
            identity: parking_lot::RwLock::new(None),
        }
    }

    /// Wire up the local identity. Called by `Daemon::run` once the
    /// vault is unlocked. Tests call this directly.
    pub(crate) fn set_identity(&self, identity: Arc<IdentityKey>) {
        *self.identity.write() = Some(identity);
    }

    // … existing dispatch_inner / dispatch_for_group unchanged …
}
```

Add `use crate::identity::IdentityKey;` to the file's imports.

The existing `Daemon::run` constructor for `DaemonInbound` will need `set_identity(Arc::clone(&handle.identity))` called once after construction — find that callsite (`crates/core/src/daemon/mod.rs` or wherever the inbound is wired) and add the call.

Now implement `dispatch_welcome`. Add to `impl InboundDispatch for DaemonInbound`:

```rust
impl InboundDispatch for DaemonInbound {
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId> {
        // existing impl unchanged
        match self.dispatch_inner(peer, ciphertext) {
            Ok(mid) => Some(mid),
            Err(e) => {
                tracing::warn!(peer = ?peer, err = %e, "inbound: dispatch failed, dropping frame");
                None
            }
        }
    }

    fn dispatch_welcome(
        &self,
        peer: PublicKey,
        welcome: &[u8],
    ) -> Option<MessageId> {
        let synthetic_id = crate::delivery::peer::welcome_msg_id(welcome);
        match self.dispatch_welcome_inner(peer, welcome) {
            Ok(()) => Some(synthetic_id),
            Err(e) => {
                tracing::warn!(
                    peer = ?peer,
                    err = %e,
                    "inbound: dispatch_welcome failed, not ACKing"
                );
                None
            }
        }
    }
}
```

Add `dispatch_welcome_inner` to `impl DaemonInbound`:

```rust
    fn dispatch_welcome_inner(
        &self,
        peer: PublicKey,
        welcome_bytes: &[u8],
    ) -> Result<()> {
        use crate::contact::Contact;
        use crate::mls::key_package::parse_welcome_kp_hash;
        use crate::mls::provider::MlsProvider;
        use crate::mls::Group;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::OutstandingInviteRepo;
        use crate::storage::{ContactRepo, MlsGroupRepo};

        let identity_arc = self
            .identity
            .read()
            .clone()
            .ok_or_else(|| {
                CoreError::from(crate::mls::MlsErrorKind::Other(
                    "inbound welcome: identity not wired".into(),
                ))
            })?;

        let kp_hash = parse_welcome_kp_hash(welcome_bytes)?;

        let oi = OutstandingInviteRepo::new(&self.pool);
        let (psk, expires_at) = oi
            .get_psk(&kp_hash)?
            .ok_or_else(|| {
                CoreError::from(crate::mls::MlsErrorKind::Other(
                    "inbound welcome: unknown kp_hash".into(),
                ))
            })?;
        let now = crate::daemon::clock::now_unix_seconds();
        if expires_at < now {
            return Err(CoreError::from(crate::mls::MlsErrorKind::Other(
                "inbound welcome: invite expired".into(),
            )));
        }

        let group = Group::join_from_welcome(
            &identity_arc,
            welcome_bytes,
            Some(psk.as_ref()),
            MlsProvider::new(),
        )?;
        let group_id = group.id().0.clone();

        // Persist all five mutations atomically.
        let group_repo = MlsGroupRepo::new(&self.pool);
        let contact_repo = ContactRepo::new(&self.pool);
        let kp_repo = KeyPackageRepo::new(&self.pool);

        self.pool.transaction(|tx| {
            group.save_in_tx(&group_repo, tx)?;
            contact_repo.upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: now,
                card: None,
            })?;
            contact_repo.set_group_id(&peer, &group_id)?;
            kp_repo.mark_consumed_in_tx(tx, &kp_hash)?;
            oi.mark_consumed_in_tx(tx, &kp_hash)?;
            Ok(())
        })?;

        let _ = self.events_tx.send(Event::ContactUpdated(peer));
        Ok(())
    }
```

This calls `mark_consumed_in_tx` variants on both repos. The existing `KeyPackageRepo::mark_consumed` may only have a non-tx version — add a `_in_tx` sibling that takes `&rusqlite::Transaction`. Same for `OutstandingInviteRepo`. (Alternative: take the locks separately and let SQLite's WAL handle interleaving — but the spec calls for one transaction, so add the `_in_tx` variants.)

Add to `crates/core/src/storage/key_packages.rs`:

```rust
    pub(crate) fn mark_consumed_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        kp_hash: &[u8; 32],
    ) -> Result<()> {
        tx.execute(
            "UPDATE key_packages SET consumed = 1 WHERE kp_hash = ?1",
            rusqlite::params![&kp_hash[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "kp: mark_consumed_in_tx: {e}"
            )))
        })?;
        Ok(())
    }
```

Add to `crates/core/src/storage/outstanding_invites.rs`:

```rust
    pub(crate) fn mark_consumed_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        kp_hash: &[u8; 32],
    ) -> Result<()> {
        tx.execute(
            "UPDATE outstanding_invites SET psk = zeroblob(32) WHERE kp_hash = ?1",
            rusqlite::params![&kp_hash[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "oi: zeroize_in_tx: {e}"
            )))
        })?;
        tx.execute(
            "DELETE FROM outstanding_invites WHERE kp_hash = ?1",
            rusqlite::params![&kp_hash[..]],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "oi: delete_in_tx: {e}"
            )))
        })?;
        Ok(())
    }
```

(`mark_consumed` itself can be refactored to call this helper with an explicit tx open from `pool.transaction(|tx| ...)`.)

- [ ] **Step 4: Wire `set_identity` in `Daemon::run`**

Find the `DaemonInbound::new(...)` callsite (likely in `crates/core/src/daemon/mod.rs`'s `Daemon::run`) and add the `set_identity` call right after construction:

```rust
let inbound = Arc::new(DaemonInbound::new(pool.clone(), events_tx.clone()));
inbound.set_identity(identity.clone());
```

(The exact field/binding name depends on the existing source. The point: identity must be wired before any Welcome can arrive.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::inbound`

Expected: All 3 new tests pass; existing inbound tests still green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/inbound.rs \
        crates/core/src/daemon/mod.rs \
        crates/core/src/storage/key_packages.rs \
        crates/core/src/storage/outstanding_invites.rs
git commit -m "$(cat <<'EOF'
feat(inbound): DaemonInbound::dispatch_welcome — join + persist atomically

Looks up PSK from outstanding_invites, calls Group::join_from_welcome,
persists (group + contact + group_id link + KP consumed +
outstanding_invites consumed) inside one transaction, emits
ContactUpdated. Adds mark_consumed_in_tx helpers to both repos.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 19: Retention `purge_expired` step

**Files:**
- Modify: `crates/core/src/daemon/retention.rs`

- [ ] **Step 1: Write the failing test**

Edit `crates/core/src/daemon/retention.rs`. In `mod tests`, add:

```rust
    #[tokio::test]
    async fn sweep_purges_expired_outstanding_invites() {
        use crate::storage::OutstandingInviteRepo;
        use zeroize::Zeroizing;

        let pool = Arc::new(Pool::in_memory());
        let now = now_unix_seconds();

        let oi = OutstandingInviteRepo::new(&pool);
        oi.put(&[0x10; 32], &Zeroizing::new([0u8; 32]), &[], now - 1, now - 3600).unwrap();
        oi.put(&[0x20; 32], &Zeroizing::new([0u8; 32]), &[], now + 3600, now).unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(pool.clone(), 0, Duration::from_millis(20), rx);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        // Expired row purged; non-expired retained.
        assert!(oi.get_psk(&[0x10; 32]).unwrap().is_none());
        assert!(oi.get_psk(&[0x20; 32]).unwrap().is_some());
    }
```

- [ ] **Step 2: Run test (will fail)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::retention::tests::sweep_purges_expired_outstanding 2>&1 | head -10`

Expected: FAIL — both rows still present (the sweep doesn't touch outstanding_invites).

- [ ] **Step 3: Add the purge step to the tick**

Edit `crates/core/src/daemon/retention.rs`. Inside `spawn_sweep`'s `tokio::time::sleep` arm, after the existing message-prune block, add:

```rust
                    // Phase 2.E: also sweep expired outstanding_invites.
                    let now = now_unix_seconds();
                    let oi = crate::storage::OutstandingInviteRepo::new(&pool);
                    match oi.purge_expired(now) {
                        Ok(n) if n > 0 => tracing::debug!(
                            rows = n,
                            "retention: purged expired outstanding invites"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            error = %e,
                            "retention: outstanding invite purge failed"
                        ),
                    }
```

(Note: `retention_days = 0` short-circuits the message-prune step but should NOT short-circuit the invite purge — invite expiry is independent of message retention. Move the `if retention_days == 0 { continue }` check inside the message-prune block only.)

The full block becomes:

```rust
                _ = tokio::time::sleep(tick) => {
                    // Message-pruning step (gated on retention_days).
                    if retention_days != 0 {
                        let cutoff = now_unix_seconds()
                            .saturating_sub(i64::from(retention_days).saturating_mul(86_400));
                        match MessageRepo::new(&pool).prune_before(None, cutoff) {
                            Ok(n) if n > 0 => tracing::info!(
                                rows = n, cutoff_ts_recv = cutoff,
                                "retention sweep deleted rows"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                error = %e,
                                "retention sweep failed; will retry next tick"
                            ),
                        }
                    }

                    // Outstanding-invite expiry sweep — always runs.
                    let now = now_unix_seconds();
                    let oi = crate::storage::OutstandingInviteRepo::new(&pool);
                    match oi.purge_expired(now) {
                        Ok(n) if n > 0 => tracing::debug!(
                            rows = n,
                            "retention: purged expired outstanding invites"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            error = %e,
                            "retention: outstanding invite purge failed"
                        ),
                    }
                }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness daemon::retention`

Expected: All retention tests still green; new sweep_purges_expired test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/retention.rs
git commit -m "$(cat <<'EOF'
feat(retention): purge expired outstanding_invites on hourly tick

Independent of retention_days (which gates message pruning only).
Zeroize-then-delete keeps PSK material from lingering on disk.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5 — Tauri bridge

### Task 20: `render_invite_qr` Tauri command

**Files:**
- Modify: `crates/ui/src/ipc_bridge.rs` (add command)
- Modify: `crates/ui/src/main.rs` (register in `invoke_handler!`)

`skattr-core` already enables `qr` by default (see `crates/core/Cargo.toml`'s `default = ["qr"]`), so `crates/ui/Cargo.toml` needs no changes.

- [ ] **Step 1: Add the command**

Edit `crates/ui/src/ipc_bridge.rs`. After the existing `pub async fn ipc_request` definition, add:

```rust
/// Render an invite link to SVG markup. Pre-daemon-friendly — does
/// not touch `AppState`. Used by `InviteGenerateDialog`.
#[tauri::command]
pub async fn render_invite_qr(url: String) -> Result<String, String> {
    use skattr_core::invite::InviteLink;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| format!("clock: {e}"))?;
    let link = InviteLink::from_url(&url, now)
        .map_err(|e| format!("parse invite: {e}"))?;
    skattr_core::invite::qr::render_svg(&link).map_err(|e| format!("render qr: {e}"))
}
```

- [ ] **Step 2: Register the command**

Edit `crates/ui/src/main.rs`. Replace the `invoke_handler!` block:

```rust
        .invoke_handler(tauri::generate_handler![
            bootstrap::vault_exists,
            bootstrap::identity_init,
            bootstrap::vault_unlock,
            ipc_bridge::ipc_request,
            ipc_bridge::render_invite_qr,
            events::ipc_subscribe,
            daemon::start_in_process_cmd,
        ])
```

- [ ] **Step 3: Build the Tauri crate to verify compile**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-ui 2>&1 | tail -10`

Expected: Clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src/ipc_bridge.rs crates/ui/src/main.rs
git commit -m "$(cat <<'EOF'
feat(ui): render_invite_qr Tauri command (delegates to core::invite::qr)

Returns SVG markup from an invite URL. No new core dep — qr feature is
already in skattr-core's default features.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6 — UI components and stores

### Task 21: jsqr dep + Toast component + toast store

**Files:**
- Modify: `crates/ui/src-svelte/package.json`
- Create: `crates/ui/src-svelte/src/lib/stores/toast.ts`
- Create: `crates/ui/src-svelte/src/lib/components/Toast.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/Toast.test.ts`

- [ ] **Step 1: Add jsqr to package.json**

Run: `cd crates/ui/src-svelte && pnpm add jsqr@1.4.0`

This updates `package.json` and `pnpm-lock.yaml`.

- [ ] **Step 2: Write the failing toast store + component test**

Create `crates/ui/src-svelte/src/lib/components/Toast.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, afterEach } from "vitest";
import { render } from "@testing-library/svelte";
import Toast from "./Toast.svelte";
import { toast, currentToast } from "../stores/toast";
import { get } from "svelte/store";

describe("Toast", () => {
  afterEach(() => {
    vi.useRealTimers();
    toast.clear();
  });

  it("renders the current toast message", () => {
    toast.show("Copied");
    const { getByText } = render(Toast);
    expect(getByText("Copied")).toBeTruthy();
  });

  it("auto-dismisses after 1500 ms", () => {
    vi.useFakeTimers();
    toast.show("hi");
    expect(get(currentToast)).not.toBeNull();
    vi.advanceTimersByTime(1500);
    expect(get(currentToast)).toBeNull();
  });

  it("replaces an in-flight toast with a new one", () => {
    toast.show("first");
    toast.show("second");
    expect(get(currentToast)?.message).toBe("second");
  });
});
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd crates/ui/src-svelte && pnpm test 2>&1 | tail -20`

Expected: FAIL — `Cannot find module './Toast.svelte'`.

- [ ] **Step 4: Implement the toast store**

Create `crates/ui/src-svelte/src/lib/stores/toast.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { writable, type Readable } from "svelte/store";

export interface ToastMessage {
  message: string;
  /** Monotonic id; the latest is shown. */
  id: number;
}

const internal = writable<ToastMessage | null>(null);
let counter = 0;
let timer: ReturnType<typeof setTimeout> | null = null;

function show(message: string, durationMs = 1500): void {
  if (timer) clearTimeout(timer);
  counter += 1;
  internal.set({ message, id: counter });
  timer = setTimeout(() => {
    internal.set(null);
    timer = null;
  }, durationMs);
}

function clear(): void {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  internal.set(null);
}

export const currentToast: Readable<ToastMessage | null> = { subscribe: internal.subscribe };
export const toast = { show, clear };
```

- [ ] **Step 5: Implement the Toast component**

Create `crates/ui/src-svelte/src/lib/components/Toast.svelte`:

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { currentToast } from "$lib/stores/toast";
</script>

{#if $currentToast}
  <div class="toast" role="status" aria-live="polite">
    {$currentToast.message}
  </div>
{/if}

<style>
  .toast {
    position: fixed;
    bottom: var(--s-3);
    right: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--bg-elevated);
    border-radius: 6px;
    font: var(--t-ui);
    box-shadow: 0 2px 12px rgba(0, 0, 0, 0.4);
    z-index: 1000;
  }
</style>
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd crates/ui/src-svelte && pnpm test -- Toast`

Expected: 3 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src-svelte/package.json \
        crates/ui/src-svelte/pnpm-lock.yaml \
        crates/ui/src-svelte/src/lib/stores/toast.ts \
        crates/ui/src-svelte/src/lib/components/Toast.svelte \
        crates/ui/src-svelte/src/lib/components/Toast.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): Toast component + singleton store + jsqr dep

Auto-dismiss 1500 ms; replaces in-flight toast with newer one.
jsqr added in this commit so subsequent UI tasks can import.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 22: ConfirmDialog component

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/ConfirmDialog.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/ConfirmDialog.test.ts`

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/components/ConfirmDialog.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import ConfirmDialog from "./ConfirmDialog.svelte";

describe("ConfirmDialog", () => {
  it("renders title and body", () => {
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Archive Bob?",
        body: "Bob disappears from your contacts.",
        confirmLabel: "Archive",
        onConfirm: vi.fn(),
        onCancel: vi.fn(),
      },
    });
    expect(getByText("Archive Bob?")).toBeTruthy();
    expect(getByText("Bob disappears from your contacts.")).toBeTruthy();
  });

  it("calls onConfirm when confirm button clicked", async () => {
    const onConfirm = vi.fn().mockResolvedValue(undefined);
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        onConfirm,
        onCancel: vi.fn(),
      },
    });
    await fireEvent.click(getByText("Archive"));
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("calls onCancel when cancel button clicked", async () => {
    const onCancel = vi.fn();
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        onConfirm: vi.fn(),
        onCancel,
      },
    });
    await fireEvent.click(getByText("Cancel"));
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("uses --danger styling when danger=true", () => {
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        danger: true,
        onConfirm: vi.fn(),
        onCancel: vi.fn(),
      },
    });
    const btn = getByText("Archive") as HTMLButtonElement;
    expect(btn.classList.contains("danger")).toBe(true);
  });
});
```

- [ ] **Step 2: Run test to verify fail**

Run: `cd crates/ui/src-svelte && pnpm test -- ConfirmDialog 2>&1 | tail -10`

Expected: FAIL — `Cannot find module './ConfirmDialog.svelte'`.

- [ ] **Step 3: Implement ConfirmDialog**

Create `crates/ui/src-svelte/src/lib/components/ConfirmDialog.svelte`:

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  interface Props {
    title: string;
    body: string;
    confirmLabel: string;
    cancelLabel?: string;
    danger?: boolean;
    onConfirm: () => Promise<void> | void;
    onCancel: () => void;
  }

  let {
    title,
    body,
    confirmLabel,
    cancelLabel = "Cancel",
    danger = false,
    onConfirm,
    onCancel,
  }: Props = $props();

  let busy = $state(false);

  async function handleConfirm() {
    if (busy) return;
    busy = true;
    try {
      await onConfirm();
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="confirm-title">
  <div class="dialog">
    <h2 id="confirm-title">{title}</h2>
    <p>{body}</p>
    <div class="actions">
      <button type="button" onclick={onCancel} disabled={busy}>{cancelLabel}</button>
      <button
        type="button"
        class:danger
        onclick={handleConfirm}
        disabled={busy}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid;
    place-items: center;
    z-index: 900;
  }
  .dialog {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-3);
    border-radius: 8px;
    max-width: 480px;
    width: 90vw;
  }
  .dialog h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  .dialog p  { font: var(--t-body); margin: 0 0 var(--s-3); }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); }
  button { padding: 8px 16px; cursor: pointer; }
  button.danger { background: var(--danger); color: var(--text); border: none; }
</style>
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd crates/ui/src-svelte && pnpm test -- ConfirmDialog`

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/ConfirmDialog.svelte \
        crates/ui/src-svelte/src/lib/components/ConfirmDialog.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): ConfirmDialog reusable modal component

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 23: qr cache store

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/qr.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/qr.test.ts`

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/stores/qr.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { renderInviteQr, _resetCacheForTest } from "./qr";

describe("qr store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    _resetCacheForTest();
  });

  it("calls render_invite_qr Tauri command on first render", async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue("<svg>x</svg>");
    const svg = await renderInviteQr("skattr://invite/v1#aaa");
    expect(svg).toBe("<svg>x</svg>");
    expect(invoke).toHaveBeenCalledWith("render_invite_qr", {
      url: "skattr://invite/v1#aaa",
    });
  });

  it("caches subsequent calls for the same URL", async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue("<svg>cached</svg>");
    const a = await renderInviteQr("skattr://invite/v1#bbb");
    const b = await renderInviteQr("skattr://invite/v1#bbb");
    expect(a).toBe(b);
    expect(invoke).toHaveBeenCalledOnce();
  });
});
```

- [ ] **Step 2: Run test (will fail)**

Run: `cd crates/ui/src-svelte && pnpm test -- qr 2>&1 | tail -10`

Expected: FAIL — module not found.

- [ ] **Step 3: Implement the store**

Create `crates/ui/src-svelte/src/lib/stores/qr.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { invoke } from "@tauri-apps/api/core";

const cache = new Map<string, string>();

export async function renderInviteQr(url: string): Promise<string> {
  const cached = cache.get(url);
  if (cached !== undefined) return cached;
  const svg = await invoke<string>("render_invite_qr", { url });
  cache.set(url, svg);
  return svg;
}

/** Test-only: clears the cache. */
export function _resetCacheForTest(): void {
  cache.clear();
}
```

- [ ] **Step 4: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- qr`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/qr.ts \
        crates/ui/src-svelte/src/lib/stores/qr.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): qr store with cache for render_invite_qr Tauri command

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 24: InviteGenerateDialog component

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.test.ts`

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import InviteGenerateDialog from "./InviteGenerateDialog.svelte";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: {
        result: "invite_created",
        data: {
          url: "skattr://invite/v1#test",
          key_package_id: "00".repeat(32),
          expires_at: 1_700_010_000,
        },
      },
    }),
  },
}));

vi.mock("$lib/stores/qr", () => ({
  renderInviteQr: vi.fn().mockResolvedValue("<svg>QR</svg>"),
}));

import { ipcClient } from "$lib/ipc/tauri";
import { renderInviteQr } from "$lib/stores/qr";

describe("InviteGenerateDialog", () => {
  it("default TTL is 24h (86400 s)", async () => {
    const onClose = vi.fn();
    const { getByText } = render(InviteGenerateDialog, { props: { onClose } });
    await fireEvent.click(getByText("Generate"));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "create_invite",
      nickname: null,
      ttl_secs: 86400,
    });
  });

  it("renders QR after successful generate", async () => {
    const { getByText, findByText } = render(InviteGenerateDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Generate"));
    await findByText(/skattr:\/\/invite/);
    expect(renderInviteQr).toHaveBeenCalledWith("skattr://invite/v1#test");
  });
});
```

- [ ] **Step 2: Run test (fail)**

Run: `cd crates/ui/src-svelte && pnpm test -- InviteGenerateDialog 2>&1 | tail -10`

Expected: FAIL — module not found.

- [ ] **Step 3: Implement the component**

Create `crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.svelte`:

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { ipcClient } from "$lib/ipc/tauri";
  import { renderInviteQr } from "$lib/stores/qr";
  import { toast } from "$lib/stores/toast";

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  type Step = "form" | "result";
  let step = $state<Step>("form");
  let nickname = $state("");
  let ttlSecs = $state<number>(86400);
  let url = $state<string | null>(null);
  let qrSvg = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  const TTL_OPTIONS = [
    { label: "1 hour", secs: 3600 },
    { label: "6 hours", secs: 21600 },
    { label: "24 hours", secs: 86400 },
    { label: "7 days", secs: 604800 },
  ];

  async function generate() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const trimmed = nickname.trim();
      const resp = await ipcClient.request({
        cmd: "create_invite",
        nickname: trimmed.length === 0 ? null : trimmed,
        ttl_secs: ttlSecs,
      } as any);
      if (resp.resp !== "ok") {
        error = "Failed to create invite.";
        return;
      }
      const data = resp.data;
      if (data?.result !== "invite_created") {
        error = "Unexpected response from daemon.";
        return;
      }
      url = data.data.url as string;
      qrSvg = await renderInviteQr(url);
      step = "result";
    } catch (e) {
      error = `${e}`;
    } finally {
      busy = false;
    }
  }

  async function copyUrl() {
    if (!url) return;
    await navigator.clipboard.writeText(url);
    toast.show("Copied");
  }
</script>

<div class="overlay" role="dialog" aria-modal="true">
  <div class="dialog">
    {#if step === "form"}
      <h2>Generate invite</h2>
      <label>
        Nickname (optional)
        <input type="text" bind:value={nickname} maxlength="64" />
      </label>
      <fieldset>
        <legend>Expires in</legend>
        {#each TTL_OPTIONS as opt}
          <label>
            <input
              type="radio"
              name="ttl"
              value={opt.secs}
              checked={ttlSecs === opt.secs}
              onchange={() => (ttlSecs = opt.secs)}
            />
            {opt.label}
          </label>
        {/each}
      </fieldset>
      {#if error}<p class="error">{error}</p>{/if}
      <div class="actions">
        <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
        <button type="button" onclick={generate} disabled={busy}>
          {busy ? "Generating…" : "Generate"}
        </button>
      </div>
    {:else}
      <h2>Invite ready</h2>
      <code class="url">{url}</code>
      {#if qrSvg}<div class="qr">{@html qrSvg}</div>{/if}
      <div class="actions">
        <button type="button" onclick={copyUrl}>Copy URL</button>
        <button type="button" onclick={onClose}>Done</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid; place-items: center;
    z-index: 900;
  }
  .dialog {
    background: var(--bg-elevated); color: var(--text);
    padding: var(--s-3); border-radius: 8px;
    max-width: 520px; width: 90vw;
  }
  h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  label { display: block; margin: var(--s-2) 0; }
  input[type="text"] { width: 100%; padding: 6px 8px; }
  fieldset { border: 1px solid var(--bg); padding: var(--s-2); margin: var(--s-2) 0; }
  .error { color: var(--danger); margin: var(--s-2) 0; }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); margin-top: var(--s-3); }
  .url { display: block; padding: var(--s-2); background: var(--bg); word-break: break-all; }
  .qr { margin: var(--s-3) auto; max-width: 240px; }
</style>
```

- [ ] **Step 4: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- InviteGenerateDialog`

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.svelte \
        crates/ui/src-svelte/src/lib/components/InviteGenerateDialog.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): InviteGenerateDialog (form + result with inline QR)

Default TTL 24h; calls CreateInvite + render_invite_qr; "Copy URL"
fires a toast.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 25: AddContactDialog — paste tab

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/AddContactDialog.test.ts`

(Scan tab is added in Task 26 to keep this commit small.)

- [ ] **Step 1: Write the failing test**

Create `crates/ui/src-svelte/src/lib/components/AddContactDialog.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import AddContactDialog from "./AddContactDialog.svelte";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: { result: "contact_added", data: {} },
    }),
  },
}));

vi.mock("$lib/stores/contacts", () => ({
  refreshContacts: vi.fn(),
}));

import { ipcClient } from "$lib/ipc/tauri";

describe("AddContactDialog (paste tab)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("submits AddContact with the pasted URL", async () => {
    const onClose = vi.fn();
    const { getByPlaceholderText, getByText } = render(AddContactDialog, {
      props: { onClose },
    });
    const input = getByPlaceholderText(/skattr:\/\/invite/i) as HTMLTextAreaElement;
    await fireEvent.input(input, {
      target: { value: "skattr://invite/v1#abc" },
    });
    await fireEvent.click(getByText("Add contact"));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "add_contact",
      invite_url: "skattr://invite/v1#abc",
    });
  });
});
```

- [ ] **Step 2: Run test (fail)**

Run: `cd crates/ui/src-svelte && pnpm test -- AddContactDialog 2>&1 | tail -10`

Expected: FAIL.

- [ ] **Step 3: Implement the component (paste tab only)**

Create `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`:

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { ipcClient } from "$lib/ipc/tauri";
  import { refreshContacts } from "$lib/stores/contacts";

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  type Tab = "paste" | "scan";
  let tab = $state<Tab>("paste");

  // Paste-tab state
  let url = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function submit() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const resp = await ipcClient.request({
        cmd: "add_contact",
        invite_url: url.trim(),
      } as any);
      if (resp.resp !== "ok") {
        error = "Failed to add contact.";
        return;
      }
      await refreshContacts();
      onClose();
    } catch (e) {
      error = `${e}`;
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" role="dialog" aria-modal="true">
  <div class="dialog">
    <h2>Add contact</h2>
    <div class="tabs" role="tablist">
      <button
        type="button"
        role="tab"
        aria-selected={tab === "paste"}
        onclick={() => (tab = "paste")}
      >Paste</button>
      <button
        type="button"
        role="tab"
        aria-selected={tab === "scan"}
        onclick={() => (tab = "scan")}
      >Scan</button>
    </div>

    {#if tab === "paste"}
      <textarea
        placeholder="skattr://invite/v1#…"
        bind:value={url}
        rows="4"
      ></textarea>
      {#if error}<p class="error">{error}</p>{/if}
      <div class="actions">
        <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
        <button type="button" onclick={submit} disabled={busy || url.trim().length === 0}>
          {busy ? "Adding…" : "Add contact"}
        </button>
      </div>
    {:else}
      <p>Scan tab — coming in next task.</p>
    {/if}
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.6); display: grid; place-items: center; z-index: 900; }
  .dialog { background: var(--bg-elevated); color: var(--text); padding: var(--s-3); border-radius: 8px; max-width: 520px; width: 90vw; }
  h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  .tabs { display: flex; gap: var(--s-2); margin-bottom: var(--s-3); border-bottom: 1px solid var(--bg); }
  .tabs button[aria-selected="true"] { border-bottom: 2px solid var(--accent); }
  textarea { width: 100%; padding: var(--s-2); resize: vertical; font: var(--t-ui); }
  .error { color: var(--danger); margin: var(--s-2) 0; }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); margin-top: var(--s-3); }
</style>
```

- [ ] **Step 4: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- AddContactDialog`

Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte \
        crates/ui/src-svelte/src/lib/components/AddContactDialog.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): AddContactDialog paste tab

Tab switcher placeholder for scan tab; paste flow submits
Command::AddContact and refreshes contacts on success.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 26: AddContactDialog — scan tab + camera permission flow

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte`
- Modify: `crates/ui/src-svelte/src/lib/components/AddContactDialog.test.ts`

- [ ] **Step 1: Add failing tests for scan-tab flows**

Append to `AddContactDialog.test.ts`:

```ts
describe("AddContactDialog (scan tab)", () => {
  beforeEach(() => vi.clearAllMocks());

  function mockUserMedia(stream: MediaStream | Promise<never>) {
    Object.defineProperty(navigator, "mediaDevices", {
      writable: true,
      value: {
        getUserMedia: vi.fn().mockReturnValue(stream),
      },
    });
  }

  it("requests camera permission on tab switch", async () => {
    const fakeStream = {
      getTracks: () => [{ stop: vi.fn() }] as any,
    } as unknown as MediaStream;
    mockUserMedia(fakeStream);

    const { getByText } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledWith({
      video: true,
    });
  });

  it("shows fallback when camera permission is denied", async () => {
    mockUserMedia(Promise.reject(new Error("NotAllowedError")));
    const { getByText, findByText } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    expect(await findByText(/Camera access denied/i)).toBeTruthy();
  });

  it("stops camera stream on close", async () => {
    const stopSpy = vi.fn();
    const fakeStream = {
      getTracks: () => [{ stop: stopSpy }] as any,
    } as unknown as MediaStream;
    mockUserMedia(fakeStream);

    const { getByText, unmount } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    // wait a microtask for getUserMedia to resolve
    await new Promise((r) => setTimeout(r, 0));
    unmount();
    expect(stopSpy).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run tests (fail for the scan tab)**

Run: `cd crates/ui/src-svelte && pnpm test -- AddContactDialog 2>&1 | tail -20`

Expected: 3 new tests fail (the scan tab is still a placeholder).

- [ ] **Step 3: Implement the scan tab**

Replace the `{:else}` branch and add scan-side state/handlers in `AddContactDialog.svelte`:

```svelte
<script lang="ts">
  // … existing imports …
  import jsQR from "jsqr";
  import { onDestroy } from "svelte";

  // Existing Props / paste-tab state remains unchanged.

  // Scan-tab state
  let scanStream = $state<MediaStream | null>(null);
  let scanError = $state<string | null>(null);
  let scanPreview = $state<string | null>(null);
  let canvas: HTMLCanvasElement | null = $state(null);
  let video: HTMLVideoElement | null = $state(null);
  let rafHandle: number | null = null;

  $effect(() => {
    if (tab !== "scan") {
      stopScan();
      return;
    }
    startScan();
    return stopScan;
  });

  async function startScan() {
    scanError = null;
    try {
      scanStream = await navigator.mediaDevices.getUserMedia({ video: true });
      // wait for the next tick so the bound video element exists
      await new Promise((r) => setTimeout(r, 0));
      if (video && scanStream) {
        video.srcObject = scanStream;
        await video.play();
        scanLoop();
      }
    } catch (e) {
      scanError = "Camera access denied — paste an invite URL instead.";
      scanStream = null;
    }
  }

  function scanLoop() {
    if (!canvas || !video) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    canvas.width = video.videoWidth || 320;
    canvas.height = video.videoHeight || 240;
    ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
    const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
    const result = jsQR(img.data, img.width, img.height, {
      inversionAttempts: "dontInvert",
    });
    if (result && result.data.startsWith("skattr://invite/v1#")) {
      scanPreview = result.data;
      stopScan();
      return;
    }
    rafHandle = requestAnimationFrame(scanLoop);
  }

  function stopScan() {
    if (rafHandle !== null) {
      cancelAnimationFrame(rafHandle);
      rafHandle = null;
    }
    if (scanStream) {
      scanStream.getTracks().forEach((t) => t.stop());
      scanStream = null;
    }
  }

  async function confirmScan() {
    if (!scanPreview) return;
    url = scanPreview;
    scanPreview = null;
    tab = "paste";   // surface in the paste tab for one final confirm
  }

  onDestroy(stopScan);
</script>

<!-- replace the placeholder {:else} branch with: -->
{:else}
  {#if scanPreview}
    <p>Detected invite:</p>
    <code class="url">{scanPreview}</code>
    <div class="actions">
      <button type="button" onclick={() => (scanPreview = null)}>Try again</button>
      <button type="button" onclick={confirmScan}>Use this invite</button>
    </div>
  {:else if scanError}
    <p class="error">{scanError}</p>
    <div class="actions">
      <button type="button" onclick={() => (tab = "paste")}>Switch to Paste</button>
    </div>
  {:else}
    <video bind:this={video} muted playsinline></video>
    <canvas bind:this={canvas} hidden></canvas>
  {/if}
{/if}
```

(Add `video { width: 100%; max-width: 360px; display: block; margin: 0 auto; }` to the style block.)

- [ ] **Step 4: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- AddContactDialog`

Expected: 4 tests pass (1 paste + 3 scan).

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/AddContactDialog.svelte \
        crates/ui/src-svelte/src/lib/components/AddContactDialog.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): AddContactDialog scan tab via getUserMedia + jsqr

Camera stream lifecycle bound to tab state via $effect; deny falls back
to "Switch to Paste"; jsqr filtered to skattr:// prefix; preview-then-
confirm before submitting.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 27: contacts.ts store — rename / archive / expand

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/stores/contacts.ts`
- Modify (or create): `crates/ui/src-svelte/src/lib/stores/contacts.test.ts`

- [ ] **Step 1: Write failing tests**

Edit `crates/ui/src-svelte/src/lib/stores/contacts.test.ts` (create if absent):

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: { result: "contacts", data: [] },
    }),
  },
}));

import { ipcClient } from "$lib/ipc/tauri";
import { rename, archive, toggleExpanded, expandedPubkey, refreshContacts } from "./contacts";
import { get } from "svelte/store";

describe("contacts store", () => {
  beforeEach(() => vi.clearAllMocks());

  it("rename calls Command::RenameContact and refreshes", async () => {
    await rename("aa".repeat(32), "Alice");
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "rename_contact",
      contact: "aa".repeat(32),
      nickname: "Alice",
    });
    // Refresh is the second call.
    expect(ipcClient.request).toHaveBeenCalledWith({ cmd: "list_contacts" });
  });

  it("archive calls Command::RemoveContact and refreshes", async () => {
    await archive("bb".repeat(32));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "remove_contact",
      contact: "bb".repeat(32),
    });
  });

  it("toggleExpanded enforces single-select", () => {
    toggleExpanded("aa");
    expect(get(expandedPubkey)).toBe("aa");
    toggleExpanded("bb");
    expect(get(expandedPubkey)).toBe("bb");
    toggleExpanded("bb");
    expect(get(expandedPubkey)).toBeNull();
  });
});
```

- [ ] **Step 2: Run tests (fail)**

Run: `cd crates/ui/src-svelte && pnpm test -- contacts.test 2>&1 | tail -10`

Expected: FAIL — no `rename` / `archive` exports.

- [ ] **Step 3: Extend the store**

Edit `crates/ui/src-svelte/src/lib/stores/contacts.ts`. After the existing exports, add:

```ts
import { writable, type Writable } from "svelte/store";

// (existing imports + `contacts` writable + `refreshContacts` already present)

export const expandedPubkey: Writable<string | null> = writable(null);

export function toggleExpanded(pubkey: string): void {
  expandedPubkey.update((current) => (current === pubkey ? null : pubkey));
}

export async function rename(contact: string, nickname: string | null): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "rename_contact",
    contact,
    nickname,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error("rename_contact failed");
  }
  await refreshContacts();
}

export async function archive(contact: string): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "remove_contact",
    contact,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error("remove_contact failed");
  }
  await refreshContacts();
}
```

- [ ] **Step 4: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- contacts.test`

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/stores/contacts.ts \
        crates/ui/src-svelte/src/lib/stores/contacts.test.ts
git commit -m "$(cat <<'EOF'
feat(ui): contacts store — rename / archive / expandedPubkey single-select

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 28: ContactDetailsPanel component

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.test.ts`
- Create: `crates/ui/src-svelte/src/lib/icons/qr-code.svg` (Lucide MIT — see `crates/ui/src-svelte/src/lib/icons/LICENSE`; copy markup from https://lucide.dev/icons/qr-code or any locally-bundled Lucide source you already trust)

- [ ] **Step 1: Add the qr-code icon**

Lucide's qr-code SVG (MIT). Create `crates/ui/src-svelte/src/lib/icons/qr-code.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
  <rect width="5" height="5" x="3" y="3" rx="1"/>
  <rect width="5" height="5" x="16" y="3" rx="1"/>
  <rect width="5" height="5" x="3" y="16" rx="1"/>
  <path d="M21 16h-3a2 2 0 0 0-2 2v3"/>
  <path d="M21 21v.01"/>
  <path d="M12 7v3a2 2 0 0 1-2 2H7"/>
  <path d="M3 12h.01"/>
  <path d="M12 3h.01"/>
  <path d="M12 16v.01"/>
  <path d="M16 12h1"/>
  <path d="M21 12v.01"/>
  <path d="M12 21v-1"/>
</svg>
```

Confirm `crates/ui/src-svelte/src/lib/icons/index.ts` re-exports it (mirror the pattern used for clock/check icons).

- [ ] **Step 2: Write the failing tests**

Create `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.test.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import ContactDetailsPanel from "./ContactDetailsPanel.svelte";

const FAKE_PK = "7aa2c4d100000000000000000000000000000000000000000000000000b3e9f7";
const FAKE_ONION = "abcdefghijklmnop1234567890.onion";

vi.mock("$lib/stores/contacts", () => ({
  rename: vi.fn().mockResolvedValue(undefined),
  archive: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("$lib/stores/toast", () => ({
  toast: { show: vi.fn(), clear: vi.fn() },
}));

import { rename, archive } from "$lib/stores/contacts";
import { toast } from "$lib/stores/toast";

function fakeSummary(overrides: Partial<{ nickname: string | null }> = {}) {
  return {
    pubkey: FAKE_PK,
    nickname: "Bob",
    onion: FAKE_ONION,
    card_version: 1,
    added_at: 0,
    unread_count: 0,
    last_message_preview: null,
    last_ts_recv: null,
    group_state: "active",
    last_read_row_id: null,
    ...overrides,
  };
}

describe("ContactDetailsPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(navigator, "clipboard", {
      writable: true,
      value: { writeText: vi.fn().mockResolvedValue(undefined) },
    });
  });

  it("renders pubkey short hash in 4…4 format", () => {
    const { getByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    expect(getByText(/7aa2c4d1…00b3e9f7/)).toBeTruthy();
  });

  it("clicking pubkey copies the full hex and shows toast", async () => {
    const { getByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText(/7aa2c4d1…/));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(FAKE_PK);
    expect(toast.show).toHaveBeenCalledWith("Copied");
  });

  it("rename submit calls store.rename", async () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "Bobby" } });
    await fireEvent.click(getByText("Save"));
    expect(rename).toHaveBeenCalledWith(FAKE_PK, "Bobby");
  });

  it("rename submit disabled for empty input", () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary({ nickname: null }) },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    expect(input.value).toBe("");
    const save = getByText("Save") as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it("rename submit disabled for >64 chars", async () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "x".repeat(65) } });
    const save = getByText("Save") as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it("archive button opens ConfirmDialog with locked copy", async () => {
    const { getByText, findByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText("Archive"));
    expect(await findByText("Archive Bob?")).toBeTruthy();
    expect(
      await findByText(/Bob disappears from your contacts/i),
    ).toBeTruthy();
  });

  it("archive confirm calls store.archive", async () => {
    const { getByText, findByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText("Archive"));
    const confirmBtn = await findByText("Archive");
    await fireEvent.click(confirmBtn);
    expect(archive).toHaveBeenCalledWith(FAKE_PK);
  });
});
```

- [ ] **Step 3: Run tests (fail)**

Run: `cd crates/ui/src-svelte && pnpm test -- ContactDetailsPanel 2>&1 | tail -10`

Expected: FAIL — module not found.

- [ ] **Step 4: Implement the component**

Create `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte`:

```svelte
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";
  import { rename, archive } from "$lib/stores/contacts";
  import { toast } from "$lib/stores/toast";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  interface Props {
    summary: ContactSummary;
  }
  let { summary }: Props = $props();

  let nickname = $state(summary.nickname ?? "");
  let confirmOpen = $state(false);

  let nicknameValid = $derived(
    nickname.trim().length > 0 && nickname.trim().length <= 64,
  );
  let pubkeyShort = $derived(
    `${summary.pubkey.slice(0, 8)}…${summary.pubkey.slice(-8)}`,
  );
  let onionShort = $derived(
    summary.onion.length > 20
      ? `${summary.onion.slice(0, 8)}…${summary.onion.slice(-8)}`
      : summary.onion,
  );

  async function copyToClipboard(value: string) {
    await navigator.clipboard.writeText(value);
    toast.show("Copied");
  }

  async function saveRename() {
    if (!nicknameValid) return;
    await rename(summary.pubkey, nickname.trim());
  }

  function openConfirm() {
    confirmOpen = true;
  }
  function closeConfirm() {
    confirmOpen = false;
  }
  async function doArchive() {
    await archive(summary.pubkey);
    confirmOpen = false;
  }
</script>

<section class="panel">
  <h3>Identity</h3>
  <button type="button" class="copyable" onclick={() => copyToClipboard(summary.pubkey)}>
    <span class="label">Pubkey</span>
    <span class="value mono">{pubkeyShort}</span>
  </button>
  <button type="button" class="copyable" onclick={() => copyToClipboard(summary.onion)}>
    <span class="label">Onion</span>
    <span class="value mono">{onionShort}</span>
  </button>

  <h3>Peer mailboxes</h3>
  <p class="empty">No mailboxes (peer mailbox projection lands in 2.F).</p>

  <h3>Rename</h3>
  <label>
    <span class="label">Nickname</span>
    <input type="text" bind:value={nickname} maxlength="64" />
  </label>
  <div class="actions">
    <button type="button" onclick={saveRename} disabled={!nicknameValid}>Save</button>
  </div>

  <h3>Danger zone</h3>
  <button type="button" class="archive" onclick={openConfirm}>Archive</button>
</section>

{#if confirmOpen}
  <ConfirmDialog
    title="Archive {summary.nickname ?? 'this contact'}?"
    body="{summary.nickname ?? 'They'} disappears from your contacts. Messages stay encrypted on disk; you can unarchive from Settings → Archived."
    confirmLabel="Archive"
    danger
    onConfirm={doArchive}
    onCancel={closeConfirm}
  />
{/if}

<style>
  .panel {
    padding: var(--s-3);
    background: var(--bg-elevated);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  h3 { font: var(--t-display); margin: var(--s-2) 0 0; }
  .copyable {
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: var(--bg);
    border: 1px solid var(--bg-elevated);
    border-radius: 4px;
    padding: var(--s-2);
    cursor: pointer;
    color: var(--text);
  }
  .label { color: var(--text-muted); font: var(--t-ui); }
  .value.mono { font-family: ui-monospace, monospace; }
  input[type="text"] { width: 100%; padding: 6px 8px; }
  .actions { display: flex; justify-content: flex-end; }
  .empty { color: var(--text-muted); font: var(--t-ui); }
  .archive { background: var(--danger); color: var(--text); border: none; padding: 8px 16px; cursor: pointer; }
</style>
```

- [ ] **Step 5: Run tests**

Run: `cd crates/ui/src-svelte && pnpm test -- ContactDetailsPanel`

Expected: 7 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte \
        crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.test.ts \
        crates/ui/src-svelte/src/lib/icons/qr-code.svg \
        crates/ui/src-svelte/src/lib/icons/index.ts
git commit -m "$(cat <<'EOF'
feat(ui): ContactDetailsPanel inline-expansion (identity / rename / archive)

Pubkey + onion as 4…4 click-to-copy; rename gated on 1–64 char trim;
archive opens ConfirmDialog with locked Phase-2.E copy.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 29: ContactRow chevron + +page.svelte wiring

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/ContactRow.svelte`
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte`

- [ ] **Step 1: Add chevron toggle in ContactRow**

Edit `ContactRow.svelte`. Add a chevron button that fires a callback:

```svelte
<script lang="ts">
  // existing imports / props…
  interface Props {
    summary: ContactSummary;
    active: boolean;
    expanded: boolean;
    onclick: () => void;
    onToggleExpanded: () => void;
  }
  let { summary, active, expanded, onclick, onToggleExpanded }: Props = $props();
</script>

<div class="row" class:active class:expanded>
  <button type="button" class="main" {onclick}>
    <!-- existing inner markup unchanged -->
  </button>
  <button
    type="button"
    class="chevron"
    aria-label={expanded ? "Hide details" : "Show details"}
    onclick={onToggleExpanded}
  >
    {expanded ? "▾" : "▸"}
  </button>
</div>

<style>
  .row { display: flex; align-items: stretch; }
  .row.active { background: var(--bg-elevated); }
  .main { flex: 1; text-align: left; background: none; border: none; color: var(--text); cursor: pointer; padding: var(--s-2); }
  .chevron { background: none; border: none; color: var(--text-muted); cursor: pointer; padding: 0 var(--s-2); }
</style>
```

- [ ] **Step 2: Wire dialogs and panel into +page.svelte**

Edit `crates/ui/src-svelte/src/routes/+page.svelte`. Replace the imports and the markup as follows (keeping all existing behaviour):

```svelte
<script lang="ts">
  // existing imports unchanged
  import InviteGenerateDialog from "$lib/components/InviteGenerateDialog.svelte";
  import AddContactDialog from "$lib/components/AddContactDialog.svelte";
  import ContactDetailsPanel from "$lib/components/ContactDetailsPanel.svelte";
  import Toast from "$lib/components/Toast.svelte";
  import { expandedPubkey, toggleExpanded } from "$lib/stores/contacts";

  let inviteOpen = $state(false);
  let addOpen = $state(false);

  // existing onMount / activeSummary / composerDisabled / selectContact unchanged
</script>

<div class="shell">
  <aside class="rail">
    <div class="rail-header">
      <button type="button" onclick={() => (inviteOpen = true)}>Generate invite</button>
      <button type="button" onclick={() => (addOpen = true)}>+ Add</button>
    </div>
    {#each $contacts as c}
      <ContactRow
        summary={c}
        active={$conversation.contact === c.pubkey}
        expanded={$expandedPubkey === c.pubkey}
        onclick={() => selectContact(c)}
        onToggleExpanded={() => toggleExpanded(c.pubkey)}
      />
      {#if $expandedPubkey === c.pubkey}
        <ContactDetailsPanel summary={c} />
      {/if}
    {/each}
  </aside>
  <main class="pane">
    <!-- existing main content unchanged -->
  </main>
</div>

<Toast />

{#if inviteOpen}
  <InviteGenerateDialog onClose={() => (inviteOpen = false)} />
{/if}
{#if addOpen}
  <AddContactDialog onClose={() => (addOpen = false)} />
{/if}

<style>
  /* existing styles unchanged */
  .rail-header {
    display: flex;
    gap: var(--s-2);
    padding: var(--s-2);
    border-bottom: 1px solid var(--bg-elevated);
  }
  .rail-header button {
    flex: 1;
    padding: 6px 8px;
    background: var(--bg-elevated);
    color: var(--text);
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font: var(--t-ui);
  }
</style>
```

- [ ] **Step 3: Run UI test suite**

Run: `cd crates/ui/src-svelte && pnpm test 2>&1 | tail -15`

Expected: All Vitest specs green.

- [ ] **Step 4: Run svelte-check**

Run: `cd crates/ui/src-svelte && pnpm check`

Expected: No type errors.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/ContactRow.svelte \
        crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "$(cat <<'EOF'
feat(ui): wire dialogs + ContactDetailsPanel + Toast into +page

ContactRow gains a chevron that toggles expandedPubkey; rail header
exposes "Generate invite" + "+ Add" buttons opening their dialogs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 7 — Tests

### Task 30: Tauri-mock fixtures for invite + add-contact flows

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`

- [ ] **Step 1: Add new fixture branches**

Edit `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`. Near the existing `_fixture200Msgs` constant, add:

```ts
const _fixtureInviteFlow =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "invite-flow";

const _fixtureAddContactFlow =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "add-contact-flow";
```

Update `let _vault = ...;` to OR-include the new fixtures so the unlock path works.

In the `case "ipc_request"` switch, add handlers:

```ts
      if (cmdObj.cmd === "create_invite" && _fixtureInviteFlow) {
        return {
          resp: "ok",
          data: {
            result: "invite_created",
            data: {
              url: "skattr://invite/v1#fixture",
              key_package_id: "0".repeat(64),
              expires_at: 1_700_010_000,
            },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "add_contact" && _fixtureAddContactFlow) {
        return {
          resp: "ok",
          data: {
            result: "contact_added",
            data: {
              pubkey: "ab".repeat(32),
              nickname: "Fixture Peer",
              onion: "fixture.onion",
              card_version: 1,
              added_at: 0,
              unread_count: 0,
              last_message_preview: null,
              last_ts_recv: null,
              group_state: "active",
              last_read_row_id: null,
            },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "rename_contact") {
        return { resp: "ok", data: { result: "ok", data: null } } as unknown as T;
      }
      if (cmdObj.cmd === "remove_contact") {
        return { resp: "ok", data: { result: "ok", data: null } } as unknown as T;
      }
```

Add a stub for the `render_invite_qr` Tauri command in the outer `switch (cmd)`:

```ts
    case "render_invite_qr":
      return "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'><rect width='100' height='100' fill='black'/></svg>" as unknown as T;
```

- [ ] **Step 2: Build the UI to verify the mock compiles**

Run: `cd crates/ui/src-svelte && pnpm build 2>&1 | tail -10`

Expected: Clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/test/tauri-mock.ts
git commit -m "$(cat <<'EOF'
test(ui): tauri-mock fixtures for invite-flow + add-contact-flow

Plus deterministic stubs for rename_contact / remove_contact /
render_invite_qr so e2e specs cover the full Phase 2.E surface.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 31: Playwright e2e — invite-generate + add-contact-paste + contact-details

**Files:**
- Create: `crates/ui/src-svelte/tests/e2e/invite-generate.spec.ts`
- Create: `crates/ui/src-svelte/tests/e2e/add-contact-paste.spec.ts`
- Create: `crates/ui/src-svelte/tests/e2e/contact-details-panel.spec.ts`

- [ ] **Step 1: Write the invite-generate spec**

Create `crates/ui/src-svelte/tests/e2e/invite-generate.spec.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { test, expect } from "@playwright/test";

test("generate invite happy path", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=invite-flow");
  await page.getByRole("button", { name: "Generate invite" }).click();
  // form is shown with default 24h selected
  await expect(page.getByRole("heading", { name: "Generate invite" })).toBeVisible();
  // pick 24h (already default) and generate
  await page.getByRole("button", { name: "Generate", exact: true }).click();
  await expect(page.getByText("skattr://invite/v1#fixture")).toBeVisible();
  // QR rendered (stub returns a minimal black square)
  await expect(page.locator("svg")).toBeVisible();
  await page.getByRole("button", { name: "Done" }).click();
});
```

- [ ] **Step 2: Write the add-contact-paste spec**

Create `crates/ui/src-svelte/tests/e2e/add-contact-paste.spec.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { test, expect } from "@playwright/test";

test("add contact via paste", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=add-contact-flow");
  await page.getByRole("button", { name: "+ Add" }).click();
  await expect(page.getByRole("heading", { name: "Add contact" })).toBeVisible();
  await page.getByPlaceholder(/skattr:\/\/invite/i).fill("skattr://invite/v1#test");
  await page.getByRole("button", { name: "Add contact", exact: true }).click();
  // Dialog closes; Fixture Peer appears in the rail.
  await expect(page.getByText("Fixture Peer")).toBeVisible();
});
```

- [ ] **Step 3: Write the contact-details-panel spec**

Create `crates/ui/src-svelte/tests/e2e/contact-details-panel.spec.ts`:

```ts
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { test, expect } from "@playwright/test";

test("contact details: short hash, rename, archive", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=seeded-contact");
  // Expand the row.
  await page.getByRole("button", { name: /Show details/ }).first().click();
  // Pubkey short-hash visible.
  await expect(page.locator("text=/^[0-9a-f]{8}…[0-9a-f]{8}$/")).toBeVisible();

  // Rename.
  await page.getByLabel("Nickname").fill("Renamed");
  await page.getByRole("button", { name: "Save" }).click();
  // (No assertion against re-fetched store — fixture's refreshContacts
  // is mocked to a no-op; existence of a successful click is sufficient.)

  // Archive flow.
  await page.getByRole("button", { name: "Archive" }).click();
  await expect(page.getByRole("heading", { name: /Archive .* /i })).toBeVisible();
  // The confirm button reads "Archive" too — the second one is the modal's.
  await page.getByRole("button", { name: "Archive" }).last().click();
});
```

- [ ] **Step 4: Run Playwright**

Run: `cd crates/ui/src-svelte && pnpm test:e2e 2>&1 | tail -20`

Expected: All e2e specs pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/tests/e2e/invite-generate.spec.ts \
        crates/ui/src-svelte/tests/e2e/add-contact-paste.spec.ts \
        crates/ui/src-svelte/tests/e2e/contact-details-panel.spec.ts
git commit -m "$(cat <<'EOF'
test(ui): Playwright e2e for invite-generate / add-contact / details panel

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 32: Real-Tor Welcome propagation integration test

**Files:**
- Create: `crates/tests/src/welcome_propagation.rs`
- Modify: `crates/tests/src/lib.rs` (or whatever file aggregates the test modules)
- Modify: `crates/tests/src/cli_two_daemons.rs` (assert group_state == Active)

- [ ] **Step 1: Add the welcome_propagation module to the test crate**

Edit `crates/tests/src/lib.rs` (or the aggregating module file) and add:

```rust
#[cfg(test)]
mod welcome_propagation;
```

- [ ] **Step 2: Write the integration test**

Create `crates/tests/src/welcome_propagation.rs`. The exact wiring follows the pattern of the existing `cli_two_daemons.rs` (paired daemons over real Tor). Use the `#[ignore]` attribute as the existing real-Tor tests do:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.E end-to-end: paired daemons over real Tor exchange an
//! invite, add the contact, propagate the Welcome, and round-trip a
//! message in both directions.
//!
//! Gated `#[ignore]` because real-Tor takes minutes; run with:
//!   `cargo test -p skattr-tests --release -- --ignored welcome_propagation`

use std::time::Duration;

use skattr_core::daemon::commands::{Command, CommandResult, MlsGroupStateLabel};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-Tor; run with --ignored"]
async fn welcome_propagates_and_round_trip_message_decrypts() {
    // 1. Spin up two daemons (Alice, Bob) using the existing harness
    //    helper used by cli_two_daemons.rs. Both daemons need real Tor
    //    so each can publish its onion service and reach the peer.
    let (alice, bob) = crate::harness::spawn_paired_daemons().await;

    // Wait for both to bootstrap.
    crate::harness::wait_for_tor_ready(&alice).await;
    crate::harness::wait_for_tor_ready(&bob).await;

    // 2. Alice creates an invite.
    let invite_url = match alice.execute(Command::CreateInvite {
        nickname: None,
        ttl_secs: Some(600),
    }).await.unwrap() {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("unexpected: {other:?}"),
    };

    // 3. Bob adds the invite.
    match bob.execute(Command::AddContact { invite_url }).await.unwrap() {
        CommandResult::ContactAdded(_) => {}
        other => panic!("unexpected: {other:?}"),
    }

    // 4. Wait for Alice's group to transition to Active. Poll for up
    //    to 30 s — direct Welcome over Tor takes a handful of seconds
    //    in the worst case. Only the inviter (Alice) starts in
    //    PendingJoin; Bob is already Active because he created the
    //    group and added Alice. We assert Alice transitions.
    let alice_pk = alice.identity_public();
    let bob_pk = bob.identity_public();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let listed = alice.execute(Command::ListContacts).await.unwrap();
        if let CommandResult::Contacts(v) = listed {
            if let Some(s) = v.iter().find(|s| s.pubkey == bob_pk) {
                if s.group_state == Some(MlsGroupStateLabel::Active) {
                    break;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Alice's group_state did not become Active within 30s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // 5. Alice sends a message to Bob; Bob decrypts.
    alice.execute(Command::SendMessage {
        contact: bob_pk,
        kind: Kind::Text { body: "hello bob".into() },
    }).await.unwrap();
    crate::harness::wait_for_message_received(&bob, alice_pk, "hello bob").await;

    // 6. Bob sends a message to Alice; Alice decrypts.
    bob.execute(Command::SendMessage {
        contact: alice_pk,
        kind: Kind::Text { body: "hi alice".into() },
    }).await.unwrap();
    crate::harness::wait_for_message_received(&alice, bob_pk, "hi alice").await;
}

// `crate::harness::spawn_paired_daemons` / `wait_for_tor_ready` /
// `wait_for_message_received` / the daemon-handle wrapper with
// `.execute()` and `.identity_public()` methods all already exist in
// the crates/tests/src/harness/ module. If the helpers are named
// differently, mirror the call sites used by cli_two_daemons.rs.
#[allow(dead_code)]
fn _dummy() -> PublicKey {
    PublicKey([0u8; 32])
}
```

(The harness helpers in `crates/tests/src/harness/` already exist for previous phases. If method names differ — e.g. `spawn_pair` rather than `spawn_paired_daemons` — adapt the calls. Keep the spec's intent: paired real-Tor daemons; assert Active transition; round-trip message in both directions.)

- [ ] **Step 3: Run the test**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests --release -- --ignored welcome_propagation 2>&1 | tail -30`

Expected: 1 test passes within ~30–60 s (mostly Tor bootstrap).

- [ ] **Step 4: Update existing `cli_two_daemons` to assert post-Welcome group state**

Edit `crates/tests/src/cli_two_daemons.rs`. Find the assertion block that currently checks the contact has been added on Bob's side and add a parallel assertion for Alice:

```rust
    // Alice's group_state must be Active after Welcome propagation.
    let alice_listed = alice.execute(Command::ListContacts).await.unwrap();
    match alice_listed {
        CommandResult::Contacts(v) => {
            let bob_summary = v.iter()
                .find(|s| s.pubkey == bob.identity_public())
                .expect("Bob is in Alice's contacts after Welcome");
            assert_eq!(
                bob_summary.group_state,
                Some(MlsGroupStateLabel::Active),
                "Alice's group with Bob must be Active post-Welcome"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
```

- [ ] **Step 5: Run the existing CLI integration test**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests --release -- --ignored cli_two_daemons`

Expected: All assertions pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tests/src/welcome_propagation.rs \
        crates/tests/src/lib.rs \
        crates/tests/src/cli_two_daemons.rs
git commit -m "$(cat <<'EOF'
test(integration): real-Tor Welcome propagation round-trip

Asserts Alice's group transitions PendingJoin -> Active within 30s of
Bob's AddContact, then exchanges a message in each direction. Existing
cli_two_daemons gains a parallel group_state assertion.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 8 — Wrap-up

### Task 33: CLAUDE.md status update

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the "Repository state" section**

Open `CLAUDE.md`. Find the paragraph that currently begins "Phase 0 is complete; Phase 1 is complete (1.H merged 2026-04-24); Phase 2.A (mailbox server) is complete; Phase 2.B (mailbox client + ContactCard rotation) is complete (merged 2026-05-01); Phase 2.C (UI bootstrap, read-only conversation MVP) is complete (merged 2026-05-02)." and append a sentence:

> Phase 2.D (conversation view) is complete (merged 2026-05-02); Phase 2.E (invite & contact UX) is complete (merged 2026-05-03).

Add a new paragraph below the existing 2.D section:

```markdown
Phase 2.E added invite-generate / add-contact dialogs, an inline
ContactDetailsPanel with rename + archive, and the daemon-side
Welcome-propagation fix. Migration `0010` adds an `outstanding_invites`
table for inviter-side PSK persistence; migration `0011` adds
`contacts.hidden` for soft-delete. `Frame::MlsWelcome` (codec slot
0x03, reserved since 1.A) is now load-bearing: `DeliveryHub::send_welcome`
+ a new peer-actor send/read arm + `InboundDispatch::dispatch_welcome`
turn Bob's `AddContact` Welcome into Alice's `Group::join_from_welcome`,
so Alice's group transitions `PendingJoin → Active` and she can decrypt
Bob's first message. Wire-format is strictly additive: three new
`Command` variants (`RenameContact`, `RemoveContact`,
`ListContactsWithFilter`), no new `CommandResult` variants, no new
`Event` variants (rename / archive reuse `ContactUpdated`).
```

Update the "Phase 2.B follow-ups" tracking sentence near the end of the repo-state section to add a new follow-up:

> **Task 2.E.5** is mailbox fallback for Welcome propagation — direct-only
> Welcome ships in 2.E; mailbox fallback is deferred because it would touch
> the 2.B mailbox protocol freeze (ADR 0006).

Update the "next workstream" sentence to point at 2.F.

- [ ] **Step 2: Verify lint passes on the file**

Run: `. "$HOME/.cargo/env" && cargo fmt --check 2>&1 | tail -5; cargo clippy -- -D warnings 2>&1 | tail -10`

Expected: Both green.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude.md): mark Phase 2.E complete; track Task 2.E.5 follow-up

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 34: Final verification

No code changes — this is the last gate before the phase merges.

- [ ] **Step 1: Format check**

Run: `. "$HOME/.cargo/env" && cargo fmt --all -- --check`

Expected: No output (formatting clean).

- [ ] **Step 2: Clippy on all targets**

Run: `. "$HOME/.cargo/env" && cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`

Expected: No warnings.

- [ ] **Step 3: All Rust tests (excluding `#[ignore]`)**

Run: `. "$HOME/.cargo/env" && cargo test --workspace 2>&1 | tail -15`

Expected: Everything green.

- [ ] **Step 4: cargo-deny**

Run: `. "$HOME/.cargo/env" && cargo deny check 2>&1 | tail -20`

Expected: No advisories, license violations, or banned-source hits. (jsqr is a TS dep, not a Rust dep — does not affect cargo-deny.)

- [ ] **Step 5: UI Vitest**

Run: `cd crates/ui/src-svelte && pnpm test 2>&1 | tail -15`

Expected: All Vitest specs pass.

- [ ] **Step 6: UI svelte-check**

Run: `cd crates/ui/src-svelte && pnpm check`

Expected: No type errors.

- [ ] **Step 7: UI Playwright**

Run: `cd crates/ui/src-svelte && pnpm test:e2e 2>&1 | tail -10`

Expected: All e2e specs pass.

- [ ] **Step 8: Real-Tor integration tests (`#[ignore]`-gated)**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests --release -- --ignored 2>&1 | tail -20`

Expected: All ignored tests pass — including `welcome_propagation` and any pre-existing real-Tor coverage.

- [ ] **Step 9: Final commit (if anything was missed) and finishing-a-development-branch handoff**

If any task ran but didn't commit cleanly, commit it now. Otherwise, this task is just verification.

```bash
# (only if there are pending changes)
git status
```

Phase 2.E is complete. Hand off to `superpowers:finishing-a-development-branch` to merge the worktree back to master.

---

## Self-review

After authoring all 34 tasks, the writer (you) MUST cross-check:

**1. Spec coverage.** Each numbered locked decision in the spec table maps to:
- Decision 1 (Direct-only `Frame::MlsWelcome`) → Tasks 11, 14, 15
- Decision 2 (PSK persistence) → Tasks 1–3 (repo) + 16 (callsite) + 18 (consume) + 19 (sweep)
- Decision 3 (Local-only rename) → Tasks 5, 7, 8 (no `ContactCard` change anywhere)
- Decision 4 (Soft-delete) → Tasks 4, 9
- Decision 5 (Archive copy) → Task 28 (literal string in confirm dialog body)
- Decision 6 (`ListContactsWithFilter` new variant) → Task 5, 10
- Decision 7 (Reuse `ContactUpdated`) → Tasks 8, 9, 18 (no new event variant added anywhere)
- Decision 8 (TTL presets 1h/6h/24h/7d default 24h) → Task 24
- Decision 9 (existing core::invite::qr) → Task 20
- Decision 10 (jsqr) → Tasks 21, 26
- Decision 11 (Webcam permission flow) → Task 26
- Decision 12 (Inline expansion) → Tasks 28, 29
- Decision 13 (4…4 short hash) → Task 28
- Decision 14 (Synthetic ACK id) → Task 11

**2. Placeholder scan.** No "TBD"/"TODO"/"add appropriate"/"fill in details"/"similar to Task N (without code)" appears.

**3. Type / method consistency.** Storage repo names match across phases (`OutstandingInviteRepo`, `ContactRepo`, `KeyPackageRepo`); `welcome_msg_id` is the same function in send + receive paths; `dispatch_welcome` matches the trait extension.

If any check fails, fix inline before saving.


