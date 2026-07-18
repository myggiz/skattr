# First-contact `PendingJoin` + durable Welcome re-send — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix first-contact state divergence (#93 / Mode B of #90): the invitee must stay `PendingJoin` until the responder Acks the Welcome, re-send the Welcome durably (never `MlsApp`) until then, and only go `Active` on Ack.

**Architecture:** The invitee's genesis group enters `GroupState::PendingJoin` (not `Active`), which — because `group.encrypt()` gates on `can_send()` — automatically blocks app frames while pending. A new durable `pending_welcomes` table + a `welcome_sweeper` task (sole Welcome-delivery path) re-sends the Welcome until the peer Acks; the Ack does an idempotent `PendingJoin→Active` CAS and deletes the row. Redaction-safe logging at every step; a non-loopback fault-injecting test drops the first Welcome to reproduce the bug the loopback guardrail can't.

**Tech Stack:** Rust 2021, rusqlite 0.38 (bundled), OpenMLS 0.8, tokio. Spec: `docs/superpowers/specs/2026-07-18-first-contact-pending-join-design.md`.

## Global Constraints

- **Cargo not on PATH** — prefix every cargo command with `. "$HOME/.cargo/env" &&`.
- **Local-first / on-demand CI** — the authoritative gate is local: `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo deny check`. CI does not auto-run.
- **No `unwrap()`/`expect()` in non-test library code.** Errors are our types (`CoreError`/`thiserror`), never a vendor's. Use `?`.
- **All `.rs` files carry the GPLv3 header** (`// SPDX-License-Identifier: GPL-3.0-or-later` / `// Copyright (C) 2026 Myggiz AB`).
- **Redaction:** never log a peer pubkey, onion, or payload bytes. Peer identity may appear only as a short redacted `Debug` if at all; prefer counts/attempt numbers. Match the existing `#90` instrumentation style.
- **rusqlite pinned at 0.38** — do not bump. Migrations are `include_str!`'d SQL keyed by `schema_version`.
- **Protocol/auth-adjacent → second reviewer** (per CLAUDE.md). No wire-format change (the Welcome bytes and frames are unchanged; only local state lifecycle + a new local table).
- **This is the invitee/committer side only.** The responder (`accept.rs` → `join_from_welcome` → Ack) is already correct; do not change it.
- **TDD:** every production change starts with a failing test.

---

### Task 1: `pending_welcomes` storage (migration `0017` + `PendingWelcomeRepo`)

Durable persistence for the Welcome to re-send. Mirrors `AttachmentDepositRepo` (`crates/core/src/storage/attachments.rs:288`) and migration `0016_attachment_deposits.sql`.

**Files:**
- Create: `crates/core/src/storage/migrations/0017_pending_welcomes.sql`
- Modify: `crates/core/src/storage/migrations.rs` (register the migration in the ordered list)
- Create: `crates/core/src/storage/pending_welcomes.rs`
- Modify: `crates/core/src/storage/mod.rs` (add `pub(crate) mod pending_welcomes;` + re-export the repo if the module re-exports repos)

**Interfaces:**
- Produces:
  - `pub struct PendingWelcomeDue { pub peer: [u8; 32], pub group_id: Vec<u8>, pub welcome_bytes: Vec<u8>, pub attempts: i64 }`
  - `pub struct PendingWelcomeRepo<'p>` with:
    - `pub fn new(pool: &'p Pool) -> Self`
    - `pub fn insert_in_tx(tx: &rusqlite::Transaction<'_>, peer: &[u8;32], group_id: &[u8], welcome_bytes: &[u8], next_retry_at_ms: i64, now_ms: i64) -> Result<()>` (associated fn, no `&self` — used inside the add_contact txn)
    - `pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<PendingWelcomeDue>>`
    - `pub fn reschedule(&self, peer: &[u8;32], next_retry_at_ms: i64) -> Result<()>` (increments `attempts`)
    - `pub fn delete(&self, peer: &[u8;32]) -> Result<()>` (idempotent)
    - `pub fn count(&self) -> Result<i64>` (for the boot-resume log line)

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0017_pending_welcomes.sql`:

```sql
-- First-contact durable Welcome re-send (#93). One row per pending first
-- contact where WE are the committer/invitee and the peer has not yet Ack'd
-- the Welcome. Deleted on Ack (peer joined) or on RemoveContact.
CREATE TABLE pending_welcomes (
    peer_pubkey    BLOB PRIMARY KEY NOT NULL,   -- responder identity pubkey (32B)
    group_id       BLOB NOT NULL,               -- genesis group id
    welcome_bytes  BLOB NOT NULL,               -- exact Welcome message to re-send
    next_retry_at  INTEGER NOT NULL,            -- ms; due-time for the next send
    attempts       INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL
);
CREATE INDEX idx_pending_welcomes_due ON pending_welcomes(next_retry_at);
```

- [ ] **Step 2: Register the migration** in `crates/core/src/storage/migrations.rs`

Find the ordered `MIGRATIONS` array (the `include_str!` list ending at `0016_attachment_deposits.sql`) and append the new entry exactly matching the existing pattern, e.g.:

```rust
    ("0017_pending_welcomes", include_str!("migrations/0017_pending_welcomes.sql")),
```

(Match the tuple/const shape already used for `0016` — read the file to copy the exact form.)

- [ ] **Step 3: Write the failing repo test**

Create `crates/core/src/storage/pending_welcomes.rs` with the license header, the structs above, and this test module (implementation `todo!()` stubbed so it compiles-then-fails):

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::Pool;

    fn pool() -> Pool {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        Pool::open(dir.path(), &seed).unwrap()
    }

    #[test]
    fn insert_due_reschedule_delete_roundtrip() {
        let p = pool();
        let repo = PendingWelcomeRepo::new(&p);
        let peer = [7u8; 32];
        p.transaction(|tx| {
            PendingWelcomeRepo::insert_in_tx(tx, &peer, &[1, 2, 3], &[9, 9, 9], 1_000, 1_000)
        })
        .unwrap();

        // Due at/after next_retry_at.
        let due = repo.due(1_000, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].peer, peer);
        assert_eq!(due[0].welcome_bytes, vec![9, 9, 9]);
        assert_eq!(due[0].attempts, 0);

        // Not due before next_retry_at.
        assert!(repo.due(999, 10).unwrap().is_empty());

        // Reschedule bumps attempts + moves the due time.
        repo.reschedule(&peer, 5_000).unwrap();
        assert!(repo.due(1_000, 10).unwrap().is_empty());
        let due2 = repo.due(5_000, 10).unwrap();
        assert_eq!(due2[0].attempts, 1);

        assert_eq!(repo.count().unwrap(), 1);
        repo.delete(&peer).unwrap();
        repo.delete(&peer).unwrap(); // idempotent
        assert_eq!(repo.count().unwrap(), 0);
    }
}
```

- [ ] **Step 4: Run it — expect FAIL** (`todo!()` panics / not implemented)

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib storage::pending_welcomes`
Expected: FAIL.

- [ ] **Step 5: Implement `PendingWelcomeRepo`** (mirror `AttachmentDepositRepo` at `attachments.rs:288-430` for the `pool.with`/`pool.with_mut` + parameterized-SQL style; `insert_in_tx` uses the `tx` directly). All SQL parameterized (no `format!`).

- [ ] **Step 6: Run — expect PASS.** Run the same command. Expected: PASS.

- [ ] **Step 7: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/storage/migrations/0017_pending_welcomes.sql crates/core/src/storage/migrations.rs crates/core/src/storage/pending_welcomes.rs crates/core/src/storage/mod.rs
git commit -m "feat(#93): pending_welcomes table + PendingWelcomeRepo (durable first-contact Welcome)"
```

---

### Task 2: Genesis group enters `PendingJoin`; add `set_active` CAS

The invitee's `add_member` genesis must produce `PendingJoin`, not `Active`; add an idempotent transition for the Ack path.

**Files:**
- Modify: `crates/core/src/mls/group.rs:175` (`add_member` sets `PendingJoin` instead of `Active`); add a `set_active` method.
- Modify: `crates/core/src/mls/state_machine.rs:23` (broaden the `PendingJoin` doc-comment).
- Modify: any test in `group.rs` / `dispatch.rs` asserting `add_member` → `Active` (update to `PendingJoin`, then `set_active` → `Active`).

**Interfaces:**
- Consumes: `GroupState::{PendingJoin, Active}` (existing, `state_machine.rs`).
- Produces:
  - `add_member(..)` leaves `self.state == GroupState::PendingJoin` (was `Active`).
  - `pub fn set_active(&mut self) -> bool` — CAS: if `PendingJoin`, set `Active { epoch: self.inner.epoch().as_u64() }` and return `true`; if already `Active`, no-op return `false`; if `Corrupt`, no-op return `false`.

- [ ] **Step 1: Write the failing test** in `group.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn add_member_leaves_pending_join_until_set_active() {
    use crate::mls::state_machine::GroupState;
    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    let alice = crate::identity::IdentityKey::generate().unwrap();
    let bob = crate::identity::IdentityKey::generate().unwrap();
    let bob_kp = crate::mls::key_package::KeyPackage::generate(&bob, &provider).unwrap();
    let mut g = Group::create_solo(&alice, None, None, &provider).unwrap();
    let _ = g.add_member(&bob_kp, None, None).unwrap();

    // Invitee/committer is NOT paired until the peer Acks the Welcome.
    assert!(matches!(g.state(), GroupState::PendingJoin), "genesis must be PendingJoin");
    assert!(!g.state().can_send(), "must not be able to send app frames while pending");

    // Ack transition is a CAS.
    assert!(g.set_active(), "first set_active flips PendingJoin -> Active");
    assert!(matches!(g.state(), GroupState::Active { .. }));
    assert!(g.state().can_send());
    assert!(!g.set_active(), "second set_active is a no-op");
}
```

> If `Group` has no `state()` accessor, add `pub fn state(&self) -> &GroupState { &self.state }` (read the struct first — there may already be one used by `save`).

- [ ] **Step 2: Run — expect FAIL** (`add_member` currently sets `Active`; `set_active` missing).

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib mls::group::tests::add_member_leaves_pending_join`
Expected: FAIL.

- [ ] **Step 3: Implement.** At `group.rs:175`, change the `add_member` genesis to:

```rust
        // First-contact committer: the group is NOT Active until the invited
        // peer processes the Welcome and Acks (join). Staying PendingJoin here
        // blocks app-frame sends (can_send()==false) so we never send MlsApp to
        // a peer that hasn't joined — see #93. The Ack path calls set_active().
        self.state = GroupState::PendingJoin;
        Ok((welcome_bytes, commit_bytes))
```

Add the CAS method (near `epoch()`):

```rust
    /// Transition PendingJoin -> Active on the peer's Welcome-Ack. Idempotent:
    /// returns true if it flipped, false if already Active (or Corrupt). #93.
    pub fn set_active(&mut self) -> bool {
        if matches!(self.state, GroupState::PendingJoin) {
            self.state = GroupState::Active { epoch: self.inner.epoch().as_u64() };
            true
        } else {
            false
        }
    }
```

Broaden the `PendingJoin` doc at `state_machine.rs:23`:

```rust
    /// The 2-party group is not yet fully established from our side: either we
    /// (the joiner) hold a Welcome we haven't processed, or we (the committer)
    /// committed the genesis but the invited peer has not joined/Ack'd yet.
    /// `can_send()` is false here so no app frames go to a not-yet-joined peer.
    PendingJoin,
```

- [ ] **Step 4: Run the new test — expect PASS.** Then run the whole mls suite to find tests that assumed `Active`-after-`add_member`:

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib mls 2>&1 | grep -E "test result:|FAILED"`
Fix any failing test that asserts `add_member` → `Active` by asserting `PendingJoin` then calling `set_active()`. (Do NOT weaken a test that legitimately checks the responder `join_from_welcome` → `Active` — that path is unchanged.)

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/mls/group.rs crates/core/src/mls/state_machine.rs
git commit -m "feat(#93): invitee genesis group is PendingJoin until Welcome-Ack (+set_active CAS)"
```

---

### Task 3: Block app sends while `PendingJoin` with a clean error

`group.encrypt()` already rejects when `!can_send()`; map that to a clear user-facing error in `send_message`.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`send_message`, ~`:517-600`, the `group.encrypt(&envelope)` call at `:592`).
- Check: `crates/core/src/error.rs` / `DaemonErrorKind` for an existing "not ready / try later" variant to reuse (e.g. `TorNotReady`-style); if none fits, the mapping produces `DaemonErrorKind::InvalidArgument { message }` with the exact copy below (no new variant unless a natural one exists).

**Interfaces:**
- Consumes: `Group::state()` / `can_send()` (Task 2).
- Produces: `send_message` to a peer whose group is `PendingJoin` returns an `Err` whose user-facing message is exactly: `"not connected yet — waiting for them to join"`.

- [ ] **Step 1: Write the failing test** (co-locate in `dispatch.rs` tests; construct a handle with a `PendingJoin` group for a peer, attempt `send_message`, assert the error message). Mirror an existing `send_message` test's harness (`dispatch.rs` tests ~`:2200+`). Assert:

```rust
    let err = send_message(&handle, peer, Kind::Text { body: "hi".into() }).await.unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("not connected yet"), "got: {msg}");
```

- [ ] **Step 2: Run — expect FAIL** (currently either sends or errors with a raw MLS message).

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch 2>&1 | grep -E "not connected|test result:"`

- [ ] **Step 3: Implement.** In `send_message`, before/at the `group.encrypt` call, guard on state:

```rust
    if !group.state().can_send() {
        // First contact still pending: the peer hasn't joined, so app frames
        // would be undeliverable/lost (#93). Surface a clear reason instead of
        // a raw MLS error; the UI shows a Connecting… (pending_join) state.
        return Err(CoreError::from(crate::daemon::commands::DaemonErrorKind::InvalidArgument {
            message: "not connected yet — waiting for them to join".into(),
        }));
    }
```

> Use whatever `DaemonErrorKind`/`CoreError` construction the surrounding code uses to reach the IPC error layer — read the nearby `map_err`/error sites in `send_message` and match them exactly. The user-facing string must be verbatim.

- [ ] **Step 4: Run — expect PASS.** Same command; the assertion passes.

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/daemon/dispatch.rs
git commit -m "feat(#93): block app sends while first contact is PendingJoin (clean error)"
```

---

### Task 4: `welcome_sweeper` — durable re-send + Ack→Active transition

The sole Welcome-delivery path: send due pending Welcomes, await Ack, and on Ack do the CAS + row-delete + self-card.

**Files:**
- Create: `crates/core/src/delivery/welcome_sweep.rs`
- Modify: `crates/core/src/delivery/mod.rs` (`pub(crate) mod welcome_sweep;`)

**Interfaces:**
- Consumes: `PendingWelcomeRepo` (Task 1), `Group::set_active` + `MlsGroupRepo` (Task 2 / `storage::groups`), `DeliveryHub::send_welcome(peer, welcome_bytes) -> Result<oneshot::Receiver<Result<(),()>>>` (existing, `delivery::hub`), `build_self_card` + `send_card_to_contact` (existing, `daemon::dispatch`).
- Produces:
  - `pub(crate) async fn run_welcome_sweep(pool: &Arc<Pool>, hub: &Arc<DeliveryHub>, handle: &DaemonHandle, now_ms: i64, batch: usize)` — one sweep pass: for each due row, send the Welcome, await the Ack (bounded timeout ~45s), on success `on_welcome_acked(...)`, else `reschedule` with backoff.
  - `pub(crate) fn welcome_backoff_ms(attempts: i64) -> i64` — bounded backoff (caps ~60_000 ms).
  - `pub(crate) async fn on_welcome_acked(handle: &DaemonHandle, peer: &[u8;32])` — load group → `set_active` (CAS) → save → `PendingWelcomeRepo::delete(peer)` → send self-card → emit `Event::ContactUpdated`. Idempotent.

> Read `delivery::chunk_sweep::run_chunk_sweep` for the exact sweep-loop shape (due → act → reschedule) and the `tracing target:` convention, and `delivery::hub::send_welcome` for the ack-receiver type. Reuse them; do not re-invent.

- [ ] **Step 1: Write a failing component test** in `welcome_sweep.rs` `#[cfg(test)]` that drives `on_welcome_acked` against a real `Pool` seeded with a `PendingJoin` group + a `pending_welcomes` row, and asserts: after the call, the group is `Active`, the row is gone, and a second call is a harmless no-op. (The full send/ack loop is covered by the Task 7 integration guardrail; here isolate the state transition.)

```rust
#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn on_welcome_acked_flips_active_and_deletes_row_idempotently() {
    // build a DaemonHandle over an in-proc Pool with a PendingJoin group for `peer`
    // + a pending_welcomes row (use the test harness helpers in daemon::dispatch tests
    // or test_exports; see loopback_harness for constructing a handle).
    // assert group PendingJoin + row present beforehand.
    on_welcome_acked(&handle, &peer).await;
    // group now Active, row deleted.
    on_welcome_acked(&handle, &peer).await; // no-op, no panic
}
```

> If constructing a bare `DaemonHandle` in a unit test is impractical, implement `on_welcome_acked`'s core as a free function taking `(&Pool, peer)` for the group+row work and test THAT directly; keep the self-card/event emission in the thin `on_welcome_acked` wrapper. Prefer the free-function split — it is cleaner to test and keeps I/O at the edge.

- [ ] **Step 2: Run — expect FAIL** (module/functions missing).

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib delivery::welcome_sweep`

- [ ] **Step 3: Implement** `run_welcome_sweep`, `welcome_backoff_ms`, `on_welcome_acked` per the interfaces, mirroring `chunk_sweep`. Redaction-safe logs (`target: "skattr::delivery::welcome_sweep"`):
  - `debug!(due = n, "welcome-sweep: {n} pending welcomes due")`
  - `debug!(attempt, "welcome-sweep: re-sending pending welcome")`
  - `info!("welcome: acked — group PendingJoin->Active")` inside `on_welcome_acked` when `set_active` returns true
  - `warn!` (via `let _ = ... ` avoidance) on a genuine save/delete error — never swallow silently.

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/delivery/welcome_sweep.rs crates/core/src/delivery/mod.rs
git commit -m "feat(#93): welcome_sweeper — durable Welcome re-send + idempotent PendingJoin->Active on Ack"
```

---

### Task 5: Wire it in — `add_contact` persists the row + nudge; spawn the sweeper; remove the in-memory retry

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs:418-499` (`add_contact`): persist the `pending_welcomes` row inside the existing genesis transaction; **delete** the in-memory `tokio::spawn` retry block (`:440-499`); nudge the sweeper.
- Modify: `crates/core/src/daemon/state.rs` (`run_with_transport`, near the `chunk_sweep_task` spawn `:438-455` and its `.abort()` at `:570`): spawn a `welcome_sweep` task on a tick + a `tokio::sync::Notify` nudge; drain on shutdown.
- Modify: `crates/core/src/delivery/hub.rs` or `DaemonHandle` — add a shared `Arc<tokio::sync::Notify>` (`welcome_nudge`) so `add_contact` can wake the sweeper for a prompt first send. (Read where `DaemonHandle`/hub shared state lives; add one field.)

**Interfaces:**
- Consumes: `PendingWelcomeRepo::insert_in_tx` (Task 1), `run_welcome_sweep` (Task 4).
- Produces: after a successful `add_contact`, a `pending_welcomes` row exists (committed with the genesis) and the sweeper delivers/re-sends the Welcome; no in-memory retry remains.

- [ ] **Step 1: Write/adjust the failing test.** Extend an existing `add_contact` test (e.g. `add_contact_from_self_invite_persists_group_link_and_emits_event`, `dispatch.rs:2219`) to assert a `pending_welcomes` row is present after `add_contact` and the group is `PendingJoin`:

```rust
    // after add_contact succeeds:
    let due = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&handle.pool)
        .due(i64::MAX, 10).unwrap();
    assert_eq!(due.len(), 1, "add_contact must persist a pending_welcome");
    // and the committer group is PendingJoin (not Active) until Ack
```

- [ ] **Step 2: Run — expect FAIL** (no row inserted yet).

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch::tests::add_contact 2>&1 | grep -E "test result:|FAILED|pending_welcome"`

- [ ] **Step 3: Implement.**
  - In the genesis transaction (`dispatch.rs:418-428`), after `contact_repo.set_group_id_in_tx(...)`, add:
    ```rust
            crate::storage::pending_welcomes::PendingWelcomeRepo::insert_in_tx(
                tx, &inviter.0, &group_id, &welcome_bytes_for_row, now_ms(), now_ms(),
            )?;
    ```
    (bind `let welcome_bytes_for_row = welcome.clone();` before the txn; `inviter` is the peer `PublicKey`, use its 32-byte array.)
  - **Delete** the entire in-memory retry block (`dispatch.rs:440-499`, the `{ let hub = ...; tokio::spawn(async move { ... welcome: gave up ... }); }`). The sweeper now owns delivery + Ack handling + self-card.
  - After the transaction commits, nudge: `handle.welcome_nudge.notify_one();`.
  - Add a redaction-safe log: `tracing::info!("first-contact: group committed PendingJoin; welcome persisted for durable re-send");`
  - In `run_with_transport` (`state.rs`), mirror the `chunk_sweep_task` block: a loop that `select!`s a tick (~5s) or the `welcome_nudge`, calls `run_welcome_sweep(&pool, &hub, &handle, now_ms(), 32)`, and on boot logs `welcome-sweep: resumed {PendingWelcomeRepo::count()} pending welcomes from durable state`. Abort/drain the task in the shutdown section next to `chunk_sweep_task.abort()`.

- [ ] **Step 4: Run — expect PASS**, then the loopback first-contact guardrail must still pass (both peers online → immediate Ack → row deleted → Active):

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch && cargo test -p skattr-tests first_contact 2>&1 | grep -E "test result:|first_contact.*ok|FAILED"`
Expected: PASS, and `first_contact_invite_add_then_bidirectional_over_loopback` still green.

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
git add crates/core/src/daemon/dispatch.rs crates/core/src/daemon/state.rs crates/core/src/delivery/hub.rs
git commit -m "feat(#93): add_contact persists pending Welcome + nudges sweeper; remove in-memory retry"
```

---

### Task 6: `RemoveContact` cancels a pending Welcome

Removing a stuck pending contact must stop the re-send.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`remove_contact`, `:1554`).

**Interfaces:**
- Consumes: `PendingWelcomeRepo::delete` (Task 1).

- [ ] **Step 1: Write the failing test.** In `dispatch.rs` tests: add a contact (→ pending_welcome row present), call `remove_contact`, assert the row is gone.

```rust
    // after add_contact: row present
    let _ = remove_contact(&handle, peer).await.unwrap();
    let due = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&handle.pool)
        .due(i64::MAX, 10).unwrap();
    assert!(due.is_empty(), "remove_contact must delete the pending_welcome");
```

- [ ] **Step 2: Run — expect FAIL.** Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::dispatch::tests::remove_contact 2>&1 | grep -E "test result:|FAILED"`

- [ ] **Step 3: Implement.** In `remove_contact`, after the existing soft-delete, add:

```rust
    // #93: stop any durable first-contact Welcome re-send for this peer.
    let _ = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&handle.pool)
        .delete(&contact.0)
        .map_err(|e| tracing::warn!(error = %e, "remove_contact: pending_welcome cleanup failed"));
```

(Keep it best-effort but logged; `RemoveContact` is a soft-delete + this cleanup.)

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings
git add crates/core/src/daemon/dispatch.rs
git commit -m "feat(#93): RemoveContact deletes any pending Welcome (cancels re-send)"
```

---

### Task 7: Integration guardrail — drop the first Welcome, prove recovery (closes the #90/#93 test gap)

A non-loopback fault-injecting transport that drops the *first* `MlsWelcome` frame, so the retry-with-`MlsApp` divergence is exercised and the durable re-send proves recovery.

**Files:**
- Create: `crates/tests/src/first_contact_welcome_dropped.rs`
- Modify: `crates/tests/src/lib.rs` (register the test module)
- Reference (read, don't edit): `crates/tests/src/first_contact_direct.rs` (the working first-contact loopback guardrail — copy its two-daemon setup), `crates/tests/src/loopback_harness.rs`, `crates/core/src/transport/loopback.rs` (the `LoopbackTransport` to wrap).

**Interfaces:**
- Consumes the whole assembly via `run_with_transport` over a wrapped loopback transport (the audit rule — prove through the real assembly, not `test_exports`).

- [ ] **Step 1: Write the failing test.** Build a transport wrapper (in the test file) over `LoopbackTransport` that, for the invitee→responder direction, **drops the first frame whose type is `MlsWelcome`** (and delivers everything after). Then run first contact end-to-end and assert:

```rust
// Pseudocode shape — implement against the real harness:
// 1. Two daemons over the drop-first-Welcome transport.
// 2. Invitee add_contact(invite).
// 3. Assert the invitee's group_state is pending_join (NOT active) after the drop,
//    and that NO MlsApp frame was emitted by the invitee (the wrapper can count
//    frame types; assert 0 MlsApp before the join completes).
// 4. Let the welcome_sweeper re-send; the SECOND Welcome is delivered.
// 5. Assert first contact completes: both daemons report the contact with
//    group_state == active, and a text message round-trips both directions.
```

Name it `first_contact_recovers_after_dropped_welcome`.

- [ ] **Step 2: Run — expect FAIL** initially only if built against pre-Task-2..5 code; against the implemented code it should PASS. To confirm it actually exercises the bug, temporarily stash Task 2 (genesis→PendingJoin) and confirm this test FAILS (invitee sends MlsApp, never recovers), then restore. Document that check in the commit message.

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests first_contact_recovers_after_dropped_welcome -- --nocapture 2>&1 | grep -E "test result:|FAILED|pending_join|active"`

- [ ] **Step 3: Implement the transport wrapper + assertions** so the test passes against the full implementation. Keep the wrapper minimal (a `Transport` impl delegating to `LoopbackTransport` with a per-connection "first MlsWelcome dropped" flag + a frame-type counter for the `MlsApp==0` assertion). Frame-type inspection: the wrapper sees framed bytes; peek the type byte (see `transport/frame.rs` `FrameType` discriminants — `MlsWelcome = 0x03`, `MlsApp = 0x05`) without decrypting payloads.

- [ ] **Step 4: Run — expect PASS.**

- [ ] **Step 5: Lint + commit**

```bash
. "$HOME/.cargo/env" && cargo fmt --all && cargo clippy -p skattr-tests --all-targets --features test-harness -- -D warnings
git add crates/tests/src/first_contact_welcome_dropped.rs crates/tests/src/lib.rs
git commit -m "test(#93): non-loopback guardrail — first contact recovers after a dropped Welcome"
```

---

### Task 8: CHANGELOG + full gate + surface pending state in CLI contacts (if not already)

**Files:**
- Modify: `CHANGELOG.md` (under the `[Unreleased] — targeting v0.1.2` `### Fixed`).
- Verify (read-only): CLI `contacts --json` already exposes `group_state` (`Active`/`pending_join`) — confirm it reflects the new pending lifecycle; no code change expected.

- [ ] **Step 1: CHANGELOG entry**

```markdown
- **First contact no longer gets permanently stuck** (#93): the inviter now
  reliably appears on both sides. Previously, if the first Welcome failed to
  reach the peer (e.g. a flaky Tor circuit), the invitee wrongly considered
  itself paired and sent app messages the peer could never accept, with no
  recovery. The invitee now stays in a "Connecting…" state and durably re-sends
  the Welcome until the peer joins.
```

- [ ] **Step 2: Full local gate**

Run:
```bash
. "$HOME/.cargo/env" && cargo fmt --all --check \
 && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
 && cargo test -p skattr-core --lib \
 && cargo test -p skattr-tests \
 && cargo deny check
```
Expected: fmt clean; clippy clean; core lib green; integration green (incl. both first-contact guardrails); deny OK. Capture the summary lines.

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(#93): changelog for first-contact PendingJoin fix"
```

---

## Notes / risks

- **Responder side is untouched.** If a test seems to want a change in `accept.rs`/`join_from_welcome`, stop — that's out of scope (Mode A / responder is correct).
- **Mode A still exists.** With this fix, a first contact under Mode A's flaky circuits will now *recover* (durable re-send retries on fresh circuits) instead of getting stuck — but individual attempts can still fail until Arti (#90) is addressed. That's expected and is the synergy the spec calls out.
- **No wire-format change / no ADR-0006 touch.** Welcome bytes + all frames unchanged; only local state + a new local table. Confirm during review whether a lightweight ADR note on the first-contact state lifecycle is wanted (spec leaves it to planning — recommend a short ADR since it formalizes PendingJoin's committer semantics).
- **Migration ordering:** `0017` must be appended after `0016` in the runner; the `SchemaTooNew` downgrade guard already handles version bumps.
