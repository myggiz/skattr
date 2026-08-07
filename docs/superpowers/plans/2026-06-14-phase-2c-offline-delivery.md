# Phase 2.C — Offline Delivery: Fallback + Drain — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a message to an offline peer automatically fall back to a semi-trusted mailbox, retry until deposited, and surface on the recipient's next poll — including legitimately-delayed deposits — and make mailbox removal preserve still-held messages.

**Architecture:** Internal delivery plumbing only — wire-format / protocol neutral, no ADR. The `MailboxFallback` dependency bundle becomes a **non-generic** `Arc<MailboxFallbackShared>` so the per-peer actor, the hub, and a new background sweeper can all run the deposit logic without referencing the generic `DeliveryHub<S>` (avoids an Arc cycle). A sustained-direct-failure timer in the per-peer actor triggers `ensure_mailbox_fallback`; a dedicated sweeper re-deposits leftover mailbox-kind outbox rows with failover + backoff; the mailbox inbound path is exempted from the ±1h replay window (dedup + MLS generation + server delete carry replay resistance); RemoveMailbox drains held deposits through the existing `dispatch_mailbox`.

**Tech Stack:** Rust 2021, Tokio, rusqlite (bundled), OpenMLS, snow, ciborium. Workspace lints: no `unwrap`/`expect` in non-test code, `-D warnings`, drop guards before `.await`.

**Spec:** `docs/superpowers/specs/2026-06-14-phase-2c-offline-delivery-design.md`

---

## Conventions for every task

**Cargo isn't on PATH.** Prefix cargo with `. "$HOME/.cargo/env" &&`.

**Per-task gates (run ALL before committing a task):**

```bash
. "$HOME/.cargo/env"
cargo fmt --all
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo test -p skattr-core --features test-harness   # the lib tests
```

**Final gate (Task 7 only), single-threaded is authoritative (CI-parity):**

```bash
cargo fmt --all -- --check
cargo test -p skattr-tests -- --test-threads=1
```

**Lesson carried from 2.A:** per-task gates ran only lib tests + clippy and missed (a) `skattr-tests` breakage and (b) fmt drift. So: run `cargo fmt --all` every task, and run the full `skattr-tests` suite in Task 7.

**License header on every new `.rs` file:**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
```

---

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/core/src/storage/outbox.rs` | `OutboxRepo::due_mailbox` query | 5 |
| `crates/core/src/delivery/outbox.rs` | `OutboxEntry` carries `target_kind` + `mailbox_id`; `row_to_entry` populates them | 1 |
| `crates/core/src/delivery/receiver.rs` | `enforce_ts_window` param on `receive` / `receive_in_tx` | 2 |
| `crates/core/src/daemon/inbound.rs` | thread the window flag (mailbox=false, direct=true); remove poison TODO | 2 |
| `crates/core/src/daemon/handle.rs` | `Option<Arc<dyn InboundDispatch>>` on `DaemonHandle` | 3 |
| `crates/core/src/daemon/dispatch.rs` | RemoveMailbox drain dispatches deposits | 3 |
| `crates/core/src/delivery/hub.rs` | `MailboxFallbackShared` (non-generic); free fn `run_mailbox_fallback`; both-dialer-and-fallback constructor; `deposit_due_mailbox_rows` | 4, 5, 6 |
| `crates/core/src/delivery/peer.rs` | direct-timeout trigger; skip mailbox-kind rows in direct retry tick | 1, 6 |
| `crates/core/src/delivery/mailbox_sweeper.rs` (new) | sweeper task loop | 5 |
| `crates/core/src/daemon/state.rs` | wire fallback into hub; set handle inbound ref; spawn sweeper | 3, 4, 5 |
| `crates/core/src/lib.rs` | `test_exports` additions if needed by Task 7 | 7 |
| `crates/tests/src/offline_fallback.rs` (new) | end-to-end offline guardrail | 7 |
| `crates/tests/src/loopback_harness.rs` | helper to drive offline scenario | 7 |

**Task order:** 1 → 2 → 3 → 4 → 5 → 6 → 7. Tasks 1, 2, 3 are independent leaves and could be done in any order; 4→5→6 form the fallback spine and are ordered; 7 is last. Following the numeric order is safe.

---

## Task 1: OutboxEntry carries routing kind; direct retry tick skips mailbox rows

**Why:** `row_to_entry` (`delivery/outbox.rs:88`) drops `target_kind`/`mailbox_id`, so the per-peer direct retry tick (`delivery/peer.rs:367`) sends mailbox-kind rows as direct `Frame::MlsApp` to the peer — silent mis-delivery.

**Files:**
- Modify: `crates/core/src/delivery/outbox.rs` (struct `OutboxEntry`, `row_to_entry`)
- Modify: `crates/core/src/delivery/peer.rs` (retry tick filter, ~line 372)
- Test: both files' `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test** in `crates/core/src/delivery/outbox.rs` `mod tests`:

```rust
#[test]
fn due_entry_carries_target_kind_and_mailbox_id() {
    use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    // one direct, one mailbox row, both due
    repo.insert_direct(&[1u8; 32], &[0xAA; 16], b"d", 100).unwrap();
    repo.insert_for_mailbox(&[1u8; 32], &[0xBB; 16], 42, b"m", 100)
        .unwrap();
    let ob = Outbox::new(&pool);
    let mut entries = ob.due(200, 10).unwrap();
    entries.sort_by_key(|e| e.message_id.0);
    // 0xAA < 0xBB
    assert_eq!(entries[0].target_kind, OutboxTargetKind::Direct);
    assert_eq!(entries[0].mailbox_id, 0);
    assert_eq!(entries[1].target_kind, OutboxTargetKind::Mailbox);
    assert_eq!(entries[1].mailbox_id, 42);
}
```

- [ ] **Step 2: Run it, verify it fails to compile** (field doesn't exist):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness due_entry_carries_target_kind
```
Expected: compile error `no field target_kind on type OutboxEntry`.

- [ ] **Step 3: Add the fields and populate them.** In `crates/core/src/delivery/outbox.rs`, import the kind and extend the struct:

```rust
use crate::storage::outbox::{OutboxRepo, OutboxRow, OutboxTargetKind};
```

Add to `OutboxEntry` (after `attempts`):

```rust
    /// Whether this row delivers directly to the peer or via a mailbox.
    pub target_kind: OutboxTargetKind,
    /// Mailbox row id when `target_kind == Mailbox`; `0` for direct rows.
    pub mailbox_id: i64,
```

Extend `row_to_entry`:

```rust
fn row_to_entry(row: OutboxRow) -> OutboxEntry {
    let mut pk = [0u8; 32];
    if row.target.len() == 32 {
        pk.copy_from_slice(&row.target);
    }
    OutboxEntry {
        id: row.id,
        target: PublicKey(pk),
        payload: row.payload,
        message_id: MessageId(row.message_id),
        attempts: row.attempts,
        target_kind: row.target_kind,
        mailbox_id: row.mailbox_id,
    }
}
```

`OutboxTargetKind` already derives `Debug, Clone, Copy, PartialEq, Eq` (`storage/outbox.rs:19`), so no derive changes are needed.

- [ ] **Step 4: Run the test, verify it passes:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness due_entry_carries_target_kind
```
Expected: PASS.

- [ ] **Step 5: Write the failing test** for the peer retry-tick skip, in `crates/core/src/delivery/peer.rs` `mod tests`. The actor's retry tick must NOT send a `target_kind='mailbox'` row over the direct connection. Use the existing `spawn_full_for_test` harness pattern (see the test at `peer.rs:711`). Add:

```rust
#[tokio::test]
async fn retry_tick_skips_mailbox_kind_rows() {
    use crate::storage::outbox::OutboxRepo;
    // A live duplex conn whose peer end we read from to detect any send.
    let (a, b) = tokio::io::duplex(16 * 1024);
    let (conn_a, _peer_b, _hb) = crate::delivery::peer::tests::handshook_pair(a, b).await;
    let peer = PublicKey([0xCD; 32]);
    let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
    // A due *mailbox* row targeting `peer` — must be ignored by the direct tick.
    OutboxRepo::new(&pool)
        .insert_for_mailbox(&peer.0, &[0x01; 16], 7, b"ct", 0)
        .unwrap();
    let (_job_tx, job_rx) = tokio::sync::mpsc::channel(4);
    let handle = crate::delivery::peer::PeerConnection::spawn_full_for_test(
        peer,
        Box::new(conn_a),
        job_rx,
        pool.clone(),
        None,
    );
    // Read the peer end for ~1.5 retry ticks; assert NO MlsApp frame arrives.
    let saw_frame = read_one_frame_with_timeout(_peer_b, std::time::Duration::from_millis(1500)).await;
    assert!(saw_frame.is_none(), "mailbox-kind row must not be sent over the direct conn");
    handle.abort();
}
```

> NOTE TO IMPLEMENTER: this codebase's existing peer tests build a handshaked `AuthenticatedConnection` pair via `handshake_initiator`/`handshake_responder` (see `peer.rs:711`–`peer.rs:780` and `hub.rs:580`). Reuse that exact pattern to obtain `conn_a` and a raw readable peer end; if a `handshook_pair` / `read_one_frame_with_timeout` helper does not exist, inline the handshake + a `tokio::time::timeout(.., framed.next())` read instead of adding new helpers. Do NOT invent a public API.

- [ ] **Step 6: Run it, verify it fails** (the row is currently sent):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness retry_tick_skips_mailbox_kind_rows
```
Expected: FAIL — a frame arrives.

- [ ] **Step 7: Add the skip.** In `full_run`'s retry tick (`peer.rs`, the `for entry in due { ... }` loop near line 372), add the kind guard alongside the existing guards:

```rust
                for entry in due {
                    if pending.contains_key(&entry.message_id) { continue; }
                    if entry.target != peer { continue; }
                    // Mailbox-kind rows are the sweeper's job, never the direct path.
                    if entry.target_kind == crate::storage::outbox::OutboxTargetKind::Mailbox {
                        continue;
                    }
                    let Some(c) = conn.as_mut() else { break; };
                    // ... unchanged ...
```

- [ ] **Step 8: Run the test, verify it passes:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness retry_tick_skips_mailbox_kind_rows
```
Expected: PASS.

- [ ] **Step 9: Per-task gates** (fmt + clippy + lib tests, see Conventions). Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/delivery/outbox.rs crates/core/src/delivery/peer.rs
git commit -m "feat(2.C): outbox entry carries routing kind; direct tick skips mailbox rows

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: ts-window exemption for the mailbox path (poison fix)

**Why:** `receive_in_tx` (`receiver.rs:74`) rejects any envelope whose `ts` is outside ±1h. A legitimately-old store-and-forward deposit is `Rejected`, never deleted, and re-fetched forever (poison). Dedup on `(sender, envelope_id)` (`receiver.rs:80`) is independent of the window, so the mailbox path can safely skip the window.

**Files:**
- Modify: `crates/core/src/delivery/receiver.rs` (`receive`, `receive_in_tx`)
- Modify: `crates/core/src/daemon/inbound.rs` (`dispatch_for_group`, callers `dispatch` / `dispatch_mailbox_inner`; remove poison TODO at line 301)
- Test: `crates/core/src/delivery/receiver.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `receiver.rs` `mod tests` (mirror the existing rejection test at `receiver.rs:230`, but assert an out-of-window envelope is **accepted** when the window is not enforced):

```rust
#[test]
fn out_of_window_accepted_when_enforcement_disabled() {
    let pool = Pool::in_memory();
    let seen = SeenMessagesRepo::new(&pool);
    let messages = MessageRepo::new(&pool);
    let sender = PublicKey([0x07; 32]);
    let gid = [0x09u8; 32];
    // ts two hours in the past (outside ±1h)
    let env = test_envelope_with_ts(/* ts_ms */ 1_000_000);
    let now_ms = 1_000_000 + 2 * 3_600_000;
    let out = receive(
        &sender, &gid, env, now_ms, /* mls_generation */ 1,
        /* ts_daemon_recv */ 1_000, &seen, &messages,
        /* enforce_ts_window */ false,
    )
    .unwrap();
    assert!(matches!(out, ReceiveOutcome::New { .. }));
}
```

> NOTE TO IMPLEMENTER: reuse whatever envelope constructor the surrounding tests already use (see the test around `receiver.rs:273` for the exact `receive` call shape and how an `Envelope` is built in this module's tests). The new trailing arg is `enforce_ts_window: bool`.

- [ ] **Step 2: Run it, verify it fails to compile** (arity mismatch):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness out_of_window_accepted_when_enforcement_disabled
```
Expected: compile error — `receive` takes N arguments.

- [ ] **Step 3: Add the parameter.** In `receiver.rs`, add `enforce_ts_window: bool` as the final parameter of both `receive` and `receive_in_tx`, and gate the window check:

```rust
    if enforce_ts_window
        && envelope.ts.saturating_sub(now_ms).saturating_abs() > REPLAY_WINDOW_MS
    {
        return Ok(ReceiveOutcome::Rejected(format!(
            "ts outside ±1h window: envelope ts={}, now={}",
            envelope.ts, now_ms
        )));
    }
```

`receive` forwards the flag to `receive_in_tx` (it wraps it in a transaction). Update the doc-comment on `now_ms` to note the window is only applied when `enforce_ts_window`.

- [ ] **Step 4: Update existing callers + tests.** The existing rejection tests (`receiver.rs:230`, `:253`) must pass `true`. The in-tx receive caller is `daemon/inbound.rs`. In `inbound.rs`, give `dispatch_for_group` an `enforce_ts_window: bool` parameter and forward it into `receive_in_tx`:

```rust
    pub(crate) fn dispatch_for_group(
        &self,
        peer: PublicKey,
        group_id: &[u8],
        ciphertext: &[u8],
        enforce_ts_window: bool,
    ) -> Result<MessageId> {
```

The direct path (`dispatch`, `inbound.rs:511`) calls it with `true`; the mailbox path (`dispatch_mailbox_inner`, `inbound.rs:343`) calls it with `false`. Remove the poison TODO block at `inbound.rs:301`–`306` and replace it with a one-line note:

```rust
    /// The mailbox path passes `enforce_ts_window = false`: store-and-forward
    /// deposits are legitimately old, so replay resistance comes from the
    /// `(sender, envelope_id)` dedup + MLS generation + server delete, not the
    /// ±1h window (which still guards the live direct path).
```

- [ ] **Step 5: Run the new test + the existing receiver tests, verify all pass:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness receiver::
```
Expected: PASS (including the two pre-existing `Rejected` tests, now passing `true`).

- [ ] **Step 6: Per-task gates.** Expected: green. (Watch for other `dispatch_for_group` callers in `inbound.rs` tests — pass `true` for direct-style tests, `false` for the mailbox trial-decrypt test at `inbound.rs:1278`.)

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/receiver.rs crates/core/src/daemon/inbound.rs
git commit -m "fix(2.C): exempt mailbox path from ±1h ts replay window (poison-deposit fix)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: RemoveMailbox drain dispatches held deposits (Task 22.5)

**Why:** `handle_remove_mailbox` (`dispatch.rs:1278`) runs a final `run_one_poll_tick` but `let _ =`-discards the `FetchResponse` (`dispatch.rs:1282`), destroying held offline messages. The `dispatch_mailbox` trait method already exists (`inbound.rs:568`); the handle just needs a reference to the inbound dispatcher.

**Files:**
- Modify: `crates/core/src/daemon/handle.rs` (add `inbound` field + setter)
- Modify: `crates/core/src/daemon/state.rs` (set the inbound ref on the handle)
- Modify: `crates/core/src/daemon/dispatch.rs` (`handle_remove_mailbox` drain loop)
- Test: `crates/tests/src/remove_mailbox_drains.rs` (extend)

- [ ] **Step 1: Add an inbound field + setter to `DaemonHandle`.** In `handle.rs`, add to the struct:

```rust
    /// The inbound MLS dispatcher, shared with the delivery hub and accept
    /// loop. `Some` in production (`run_with_transport`), `None` in handle
    /// unit tests that don't exercise inbound. Used by the RemoveMailbox
    /// drain to dispatch held deposits before finalizing removal.
    inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
```

Initialize it to `None` in every `DaemonHandle` constructor (`new` / `new_with_mailbox`), and add:

```rust
    /// Inject the shared inbound dispatcher (call in `run_with_transport`).
    pub(crate) fn set_inbound(
        &mut self,
        inbound: Arc<dyn crate::delivery::peer::InboundDispatch>,
    ) {
        self.inbound = Some(inbound);
    }
```

- [ ] **Step 2: Wire it in `state.rs`.** After the handle is built (`state.rs:351`–`364`, near `handle.set_group_locks(group_locks);`), add:

```rust
    handle.set_inbound(inbound.clone());
```

(`inbound` is the `Arc<dyn InboundDispatch>` already in scope from Step 2 of `run_with_transport`.)

- [ ] **Step 3: Write the failing test** by extending `crates/tests/src/remove_mailbox_drains.rs`. The existing test (`remove_mailbox_emits_status_events_and_drains_server`) asserts server-side drain; add an assertion that a held deposit lands in the recipient's storage (a `MessageReceived` event fires / a `RecentMessages` query returns it) after `RemoveMailbox`. Model the deposit + decrypt setup on `crates/tests/src/mailbox_offline_delivery.rs` (`paired_groups` + `install_contact_with_card` + a real `Deposit`), then issue `Command::RemoveMailbox` and assert the message is now persisted.

> NOTE TO IMPLEMENTER: read `crates/tests/src/remove_mailbox_drains.rs` and `crates/tests/src/mailbox_offline_delivery.rs` in full first; reuse their helpers verbatim. The new assertion is "the drained deposit is dispatched into storage," not merely "deleted server-side."

- [ ] **Step 4: Run it, verify it fails** (deposit drained but not dispatched):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests remove_mailbox -- --test-threads=1
```
Expected: FAIL — message not persisted.

- [ ] **Step 5: Implement the drain dispatch.** In `dispatch.rs` `handle_remove_mailbox`, replace the discard block (`dispatch.rs:1278`–`1284`) with:

```rust
    // 3. Best-effort final drain. Fetch + server-side delete, AND dispatch
    //    each held deposit into local storage before we forget the mailbox.
    if let Some(factory) = &handle.mailbox_factory {
        if let Ok(mut client) = factory.connect(&row.onion).await {
            if let Ok(fetched) =
                crate::mailbox::poll::run_one_poll_tick(&mut client, &handle.identity).await
            {
                if let Some(inbound) = &handle.inbound {
                    for deposit in &fetched.deposits {
                        // dispatch_mailbox trial-decrypts, persists, and emits
                        // MessageReceived. Mailbox path → ts-window exempt (Task 2).
                        let _ = inbound.dispatch_mailbox(&deposit.ciphertext);
                    }
                }
            }
        }
    }
```

`FetchResponse.deposits: Vec<PendingDeposit>` and `PendingDeposit.ciphertext: Vec<u8>` (`mailbox/protocol.rs:117`–`130`). Remove the Task 22.5 TODO doc block at `dispatch.rs:1239`–`1243` and `1281`.

- [ ] **Step 6: Run the test, verify it passes:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests remove_mailbox -- --test-threads=1
```
Expected: PASS.

- [ ] **Step 7: Per-task gates.** Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/daemon/handle.rs crates/core/src/daemon/state.rs crates/core/src/daemon/dispatch.rs crates/tests/src/remove_mailbox_drains.rs
git commit -m "feat(2.C): RemoveMailbox drain dispatches held deposits before finalize (Task 22.5)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Make MailboxFallback shareable + construct fallback in production

**Why:** Today the production hub uses `new_with_inbound_and_dialer` (`state.rs:322`) which sets `fallback: None`; and `new_with_mailbox_fallback` sets `dialer: None`. They're mutually exclusive, so production has dial-on-demand but no fallback. We also need the fallback bundle to be a **non-generic** `Arc` so the per-peer actor (Task 6) and sweeper (Task 5) can run the deposit logic without a reference to `DeliveryHub<S>` (avoids an Arc cycle).

**Files:**
- Modify: `crates/core/src/delivery/hub.rs` (`MailboxFallback` → `Arc<MailboxFallbackShared>`; new constructor; refactor `ensure_mailbox_fallback` into a free fn)
- Modify: `crates/core/src/daemon/state.rs` (use the new constructor)
- Test: `crates/core/src/delivery/hub.rs` `mod tests` (existing fallback tests must stay green)

- [ ] **Step 1: Introduce `MailboxFallbackShared`.** In `hub.rs`, rename the current `MailboxFallback` struct (fields `factory`, `events`, `identity`) to `MailboxFallbackShared` and store it on the hub as `Option<Arc<MailboxFallbackShared>>`:

```rust
pub(crate) struct MailboxFallbackShared {
    pub(crate) factory: Arc<dyn MailboxConnectFactory>,
    pub(crate) events: broadcast::Sender<Event>,
    #[allow(dead_code)]
    pub(crate) identity: Arc<IdentityKey>,
}
```

```rust
    fallback: Option<Arc<MailboxFallbackShared>>,
```

- [ ] **Step 2: Extract the fallback body into a free async fn.** Move the body of `ensure_mailbox_fallback` (`hub.rs:374`–`524`) into:

```rust
/// Retarget the existing direct outbox row for `(peer, message_id)` to one of
/// the peer's advertised mailboxes and deposit `ciphertext`, walking the list
/// on failure. Deletes the outbox row + emits `DeliveryStatusChanged` on
/// success; leaves the (now mailbox-kind) row for the sweeper on failure.
/// Non-generic so the per-peer actor and the sweeper can call it without a
/// reference to the generic `DeliveryHub<S>`.
pub(crate) async fn run_mailbox_fallback(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    peer: PublicKey,
    message_id: MessageId,
    ciphertext: Vec<u8>,
) {
    // ... the existing body, with `self.pool` → `pool`, `fallback` → `shared` ...
}
```

Then `ensure_mailbox_fallback` becomes a thin forwarder (keeps the existing public signature the tests use):

```rust
    pub async fn ensure_mailbox_fallback(
        &self,
        peer: PublicKey,
        message_id: MessageId,
        ciphertext: Vec<u8>,
    ) {
        let Some(shared) = self.fallback.as_ref() else {
            tracing::debug!(target: "skattr::delivery::hub", "fallback skipped: hub has no mailbox factory");
            return;
        };
        run_mailbox_fallback(&self.pool, shared, peer, message_id, ciphertext).await;
    }
```

The helper free fns (`pick_first_mailbox_index`, `recipient_hash_from_pubkey` import, `FALLBACK_TTL_SECS`) are already module-level and usable from `run_mailbox_fallback`.

- [ ] **Step 3: Add a constructor carrying BOTH dialer and fallback.** In `hub.rs`:

```rust
    /// Production constructor: on-demand `dialer` AND the direct→mailbox
    /// fallback orchestrator. Used by `run_with_transport`.
    pub(crate) fn new_with_inbound_dialer_and_fallback(
        pool: Arc<Pool>,
        dispatch: Arc<dyn InboundDispatch>,
        dialer: Arc<dyn crate::delivery::dial::OutboundDial<S>>,
        fallback: Arc<MailboxFallbackShared>,
    ) -> Self {
        Self::new_inner(pool, Some(dispatch), Some(fallback), Some(dialer))
    }
```

Update `new_inner`'s `fallback` parameter type to `Option<Arc<MailboxFallbackShared>>` and update the existing `new_with_mailbox_fallback` to wrap its bundle in `Arc::new(MailboxFallbackShared { .. })`. Add a `pub(crate) fn fallback_shared(&self) -> Option<Arc<MailboxFallbackShared>>` accessor (clones the `Arc`) — Task 5/6 need it.

- [ ] **Step 4: Wire production in `state.rs`.** Replace the hub construction at `state.rs:322`–`326` with:

```rust
    let fallback_shared = Arc::new(crate::delivery::hub::MailboxFallbackShared {
        factory: mailbox_factory.clone(),
        events: events_tx.clone(),
        identity: transport_identity.clone(),
    });
    let hub: Arc<DeliveryHub<T::Stream>> =
        Arc::new(DeliveryHub::new_with_inbound_dialer_and_fallback(
            pool.clone(),
            inbound.clone(),
            dialer,
            fallback_shared.clone(),
        ));
```

Keep `fallback_shared` in scope — Task 5 hands it to the sweeper.

- [ ] **Step 5: Run the existing hub fallback tests, verify still green:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness hub::
```
Expected: PASS — `ensure_mailbox_fallback_picks_one_then_succeeds`, `..._cascades_on_first_mailbox_error`, `..._with_no_mailboxes_leaves_outbox_row` all still pass through the forwarder.

- [ ] **Step 6: Per-task gates.** Expected: green. (No behavior change yet beyond production now having `fallback: Some` — that's exercised in Task 7.)

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/delivery/hub.rs crates/core/src/daemon/state.rs
git commit -m "refactor(2.C): non-generic MailboxFallbackShared + dialer+fallback hub ctor; wire fallback in production

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Mailbox-outbox sweeper (deposit + retry engine)

**Why:** Nothing re-deposits a mailbox-kind outbox row whose first deposit failed (or that was retargeted by the timeout trigger). A dedicated background task is the single deposit/retry engine; the per-peer actor stays direct-only.

**Files:**
- Create: `crates/core/src/delivery/mailbox_sweeper.rs`
- Modify: `crates/core/src/delivery/mod.rs` (add `pub(crate) mod mailbox_sweeper;`)
- Modify: `crates/core/src/storage/outbox.rs` (`due_mailbox` query)
- Modify: `crates/core/src/daemon/state.rs` (spawn the sweeper task; abort on shutdown)
- Test: `crates/core/src/delivery/mailbox_sweeper.rs` `mod tests`

- [ ] **Step 1: Add `OutboxRepo::due_mailbox`.** In `storage/outbox.rs` (after `due`, ~line 301), filtered by kind so the sweeper doesn't load direct rows:

```rust
    /// Due rows with `target_kind='mailbox'`, oldest first, up to `limit`.
    pub(crate) fn due_mailbox(&self, now: i64, limit: usize) -> Result<Vec<OutboxRow>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, target, payload, message_id, attempts, target_kind, mailbox_id \
                     FROM outbox WHERE next_retry_at <= ?1 AND target_kind='mailbox' \
                     ORDER BY next_retry_at LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("prepare due_mailbox: {e}"))))?;
            let rows = stmt
                .query_map(rusqlite::params![now, i64::try_from(limit).unwrap_or(i64::MAX)], Self::map_row)
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("query due_mailbox: {e}"))))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("collect due_mailbox: {e}"))))
        })
    }
```

Add a unit test in `storage/outbox.rs` `mod tests`:

```rust
#[test]
fn due_mailbox_returns_only_mailbox_kind() {
    let pool = Pool::in_memory();
    let repo = OutboxRepo::new(&pool);
    repo.insert_direct(&[1u8; 32], &[0xAA; 16], b"d", 100).unwrap();
    repo.insert_for_mailbox(&[1u8; 32], &[0xBB; 16], 9, b"m", 100).unwrap();
    let rows = repo.due_mailbox(200, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target_kind, OutboxTargetKind::Mailbox);
    assert_eq!(rows[0].mailbox_id, 9);
}
```

- [ ] **Step 2: Run that test, verify it passes** (it's additive, will pass once compiled):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness due_mailbox_returns_only_mailbox_kind
```
Expected: PASS.

- [ ] **Step 3: Write the failing sweeper test.** Create `crates/core/src/delivery/mailbox_sweeper.rs` with the license header and a test that a single sweep pass deposits a due mailbox row and deletes it on success. Model the in-process mailbox server + factory on `hub.rs`'s fallback tests (`hub.rs:600`–`910`: `StubFactory`/`deposit_server`/`DepositReply`). The sweep entry point:

```rust
/// Run one sweep pass: deposit every due mailbox-kind outbox row, deleting
/// rows on success and rescheduling (with backoff) on failure. Best-effort —
/// errors are logged and the row is retained for the next pass.
pub(crate) async fn run_mailbox_sweep(
    pool: &crate::storage::Pool,
    shared: &crate::delivery::hub::MailboxFallbackShared,
    now: i64,
    batch: usize,
) {
    // ... see Step 4 ...
}
```

Test sketch:

```rust
#[tokio::test]
async fn sweep_deposits_due_mailbox_row_and_deletes_on_success() {
    // pool + a peer + a 'theirs' mailbox row mapping mailbox_id -> onion,
    // + a due mailbox outbox row for that peer/mailbox_id.
    // shared = MailboxFallbackShared { factory: stub that accepts deposit, .. }
    run_mailbox_sweep(&pool, &shared, now, 16).await;
    // assert: the outbox row is gone (deposit succeeded -> delete_by_id)
    assert!(OutboxRepo::new(&pool).due_mailbox(now + 10_000, 16).unwrap().is_empty());
}
```

> NOTE TO IMPLEMENTER: reuse the `StubFactory` + `deposit_server` test scaffolding from `hub.rs` (copy the minimal pieces into this module's `mod tests`, or lift them to a shared `#[cfg(test)]` helper if duplication is large). Do not add new production APIs just for the test.

- [ ] **Step 4: Implement `run_mailbox_sweep`.** It reuses `run_mailbox_fallback` semantics but for already-mailbox-kind rows. Resolve each row to `(peer, message_id, ciphertext)` and re-run the deposit. Because `run_mailbox_fallback` keys off `find_direct_id` (a *direct* row), the sweeper needs a per-row deposit that walks the peer's mailbox list for an existing mailbox-kind row. Add a sibling free fn in `hub.rs` that the sweeper calls:

```rust
// in hub.rs
/// Deposit a single already-mailbox-kind outbox row, walking the peer's
/// mailbox list; delete the row + emit on success, reschedule with backoff on
/// failure. Returns true on successful deposit.
pub(crate) async fn redeposit_mailbox_row(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    row: &crate::storage::outbox::OutboxRow,
    now: i64,
) -> bool {
    use crate::delivery::backoff::backoff;
    use crate::storage::{mailboxes::MailboxRepo, outbox::OutboxRepo};
    let mut peer = [0u8; 32];
    if row.target.len() == 32 { peer.copy_from_slice(&row.target); }
    let peer = PublicKey(peer);
    let onions = match MailboxRepo::new(pool).list_for_contact(&peer) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            let _ = OutboxRepo::new(pool).reschedule(
                row.id,
                now.saturating_add(i64::try_from(backoff(row.attempts).as_millis()).unwrap_or(i64::MAX)),
            );
            return false;
        }
    };
    let recipient_hash = recipient_hash_from_pubkey(&peer.0);
    let mut mid = [0u8; 16];
    mid.copy_from_slice(&row.message_id);
    for onion in &onions {
        let mut client = match shared.factory.connect(onion).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if client.deposit(recipient_hash, row.payload.clone(), FALLBACK_TTL_SECS).await.is_ok() {
            let _ = OutboxRepo::new(pool).delete_by_id(row.id);
            let _ = shared.events.send(Event::DeliveryStatusChanged {
                message: MessageId(mid),
                status: DeliveryStatus::Deposited,
            });
            return true;
        }
    }
    // all mailboxes failed: reschedule with backoff
    let _ = OutboxRepo::new(pool).reschedule(
        row.id,
        now.saturating_add(i64::try_from(backoff(row.attempts).as_millis()).unwrap_or(i64::MAX)),
    );
    false
}
```

> NOTE: `OutboxRepo::reschedule(id, next_retry_at)` takes an absolute timestamp and bumps `attempts` (`storage/outbox.rs:390`). `backoff(attempts)` is `crate::delivery::backoff::backoff`. `MailboxClient::deposit(recipient_hash, ciphertext, ttl)` (`mailbox/client.rs:49`). Confirm `now` units match `next_retry_at` (the per-peer tick uses `now_ms()`; the outbox stores millis — use millis here too: `crate::delivery::peer` uses `now_ms`; reuse the same clock helper).

Then `run_mailbox_sweep` is the loop:

```rust
pub(crate) async fn run_mailbox_sweep(
    pool: &crate::storage::Pool,
    shared: &crate::delivery::hub::MailboxFallbackShared,
    now: i64,
    batch: usize,
) {
    let rows = match crate::storage::outbox::OutboxRepo::new(pool).due_mailbox(now, batch) {
        Ok(r) => r,
        Err(e) => { tracing::warn!(target: "skattr::delivery::sweeper", error = %e, "due_mailbox failed"); return; }
    };
    for row in &rows {
        crate::delivery::hub::redeposit_mailbox_row(pool, shared, row, now).await;
    }
}
```

- [ ] **Step 5: Run the sweeper test, verify it passes:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness sweep_deposits_due_mailbox_row
```
Expected: PASS.

- [ ] **Step 6: Spawn the sweeper in `run_with_transport`.** In `state.rs`, after the poll scheduler (around `state.rs:347`), spawn a periodic task and ensure it's aborted on shutdown (mirror how `accept_task` / sweep handles are dropped). Use a fixed interval constant:

```rust
    // Mailbox-outbox sweeper: re-deposit due mailbox-kind rows (retry engine).
    let sweeper_pool = pool.clone();
    let sweeper_shared = fallback_shared.clone();
    let mailbox_sweeper_task = tokio::spawn(async move {
        const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);
        let mut t = tokio::time::interval(SWEEP_EVERY);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            t.tick().await;
            let now = crate::daemon::clock::now_unix_millis();
            crate::delivery::mailbox_sweeper::run_mailbox_sweep(&sweeper_pool, &sweeper_shared, now, 32).await;
        }
    });
```

Abort it on the shutdown path next to the other task aborts (find where `accept_task.abort()` / scheduler drop happens at the end of `run_with_transport` and add `mailbox_sweeper_task.abort();`).

> NOTE TO IMPLEMENTER: if `crate::daemon::clock::now_unix_millis` does not exist (only `now_unix_seconds` at `daemon/clock.rs`), add it next to it (`SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)`), matching the existing helper's no-`unwrap` style. The outbox `next_retry_at` is in millis (the per-peer `now_ms()` writes it), so the sweeper MUST use millis.

- [ ] **Step 7: Per-task gates.** Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/delivery/mailbox_sweeper.rs crates/core/src/delivery/mod.rs crates/core/src/delivery/hub.rs crates/core/src/storage/outbox.rs crates/core/src/daemon/state.rs crates/core/src/daemon/clock.rs
git commit -m "feat(2.C): mailbox-outbox sweeper re-deposits due mailbox rows with failover+backoff

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Direct→mailbox timeout trigger in the per-peer actor (Task 20.5)

**Why:** Nothing converts sustained direct-delivery failure into a fallback. The per-peer actor must, after `direct_timeout_secs` of unbroken failure to a peer with pending rows, hand those rows to `run_mailbox_fallback` (retarget + first deposit attempt); the sweeper (Task 5) handles subsequent retries.

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (`full_run` + `spawn` signatures; failure timer)
- Modify: `crates/core/src/delivery/hub.rs` (pass fallback + timeout when spawning actors)
- Modify: `crates/core/src/daemon/state.rs` (pass `direct_timeout` from config into the hub)
- Test: `crates/core/src/delivery/peer.rs` `mod tests`

- [ ] **Step 1: Thread fallback + timeout into the actor.** Add two parameters to `full_run` and `spawn`:

```rust
    direct_timeout: std::time::Duration,
    fallback: Option<std::sync::Arc<crate::delivery::hub::MailboxFallbackShared>>,
```

`spawn_with_conn_for_test` / `spawn_full_for_test` pass `Duration::from_secs(0)`-disabled + `None` (no behavior change). The hub's real `spawn` call site (`hub.rs:219`) passes the configured timeout and `self.fallback_shared()`.

- [ ] **Step 2: Write the failing test.** In `peer.rs` `mod tests`, drive an actor whose direct conn always fails to send, with a short `direct_timeout`, a pending direct row, and a stub fallback that records a call. Assert the fallback runs (the row is retargeted to mailbox-kind) within the timeout window:

```rust
#[tokio::test]
async fn sustained_direct_failure_triggers_fallback_retarget() {
    use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
    let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
    let peer = PublicKey([0xEE; 32]);
    // peer has one advertised mailbox so retarget can pick it
    let mb_id = crate::storage::mailboxes::MailboxRepo::new(&pool)
        .ensure_theirs("examplemailboxonionaddr.onion", 0).unwrap();
    install_theirs_for_peer(&pool, &peer, mb_id); // helper: link mailbox to contact
    OutboxRepo::new(&pool).insert_direct(&peer.0, &[0x01; 16], b"ct", 0).unwrap();

    let shared = std::sync::Arc::new(stub_fallback_shared(&pool)); // accepts deposit
    // actor with NO conn and NO dialer -> every send fails -> timer arms
    let (_job_tx, job_rx) = tokio::sync::mpsc::channel(4);
    let (_wj_tx, wj_rx) = tokio::sync::mpsc::channel(4);
    let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<PeerCtrl<tokio::io::DuplexStream>>(4);
    tokio::spawn(full_run::<tokio::io::DuplexStream>(
        peer, None, job_rx, wj_rx, ctrl_rx, pool.clone(), None, None,
        std::time::Duration::from_millis(200), Some(shared),
    ));
    // within ~1s the row should be retargeted (deposit succeeded -> row deleted,
    // or at minimum target_kind flipped to Mailbox).
    let ok = wait_until(std::time::Duration::from_secs(2), || {
        OutboxRepo::new(&pool).get_kind(&peer.0, &[0x01; 16]) != Some(OutboxTargetKind::Direct)
    }).await;
    assert!(ok, "sustained direct failure must trigger mailbox fallback");
}
```

> NOTE TO IMPLEMENTER: `get_kind` is illustrative — assert via whatever is cleanest (e.g. `due_mailbox` non-empty, or the direct row gone). Reuse the `StubFactory`/`MailboxFallbackShared` test scaffolding from `hub.rs`/Task 5. The helper `install_theirs_for_peer` should mirror how `mailbox_failover.rs`/`mailbox_offline_delivery.rs` link a `theirs` mailbox to a contact so `list_for_contact` returns it.

- [ ] **Step 3: Run it, verify it fails** (no trigger yet):

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness sustained_direct_failure_triggers_fallback
```
Expected: FAIL — row stays `Direct`.

- [ ] **Step 4: Implement the timer.** In `full_run`, track failure state and fire the fallback on expiry:

```rust
    let mut first_failure_at: Option<tokio::time::Instant> = None;
    let fallback_enabled = fallback.is_some() && direct_timeout > std::time::Duration::ZERO;
```

On every send/dial failure path (the `ack_tx.send(Err(()))` arms in the `jobs`/`welcome_jobs`/retry-tick branches), set `if first_failure_at.is_none() { first_failure_at = Some(Instant::now()); }`. On every successful send, clear it: `first_failure_at = None;`.

Add a branch to the `select!` (or check inside the existing `retry_tick`): when `fallback_enabled` and `first_failure_at.map(|t| t.elapsed() >= direct_timeout).unwrap_or(false)`, load the peer's due direct rows and run the fallback for each, then disarm:

```rust
            _ = retry_tick.tick() => {
                // ... existing direct retry loop (now skips mailbox rows, Task 1) ...

                if fallback_enabled
                    && first_failure_at.map(|t| t.elapsed() >= direct_timeout).unwrap_or(false)
                {
                    if let Some(shared) = fallback.as_ref() {
                        let ob = Outbox::new(&pool);
                        let now = now_ms();
                        if let Ok(due) = ob.due(now, 32) {
                            for e in due.into_iter().filter(|e| {
                                e.target == peer
                                    && e.target_kind == crate::storage::outbox::OutboxTargetKind::Direct
                            }) {
                                crate::delivery::hub::run_mailbox_fallback(
                                    &pool, shared, peer, e.message_id, e.payload,
                                ).await;
                            }
                        }
                    }
                    first_failure_at = None; // disarm; sweeper owns subsequent retries
                }
            }
```

> NOTE: `run_mailbox_fallback` deletes the row on a successful deposit, or leaves it mailbox-kind (retargeted) on failure — the sweeper retries from there. The guard drop / no-await-holding-lock rule doesn't apply here (no lock held), but keep `pool` access via short-lived `Outbox`/repo handles.

- [ ] **Step 5: Update the hub's actor spawn** (`hub.rs:219`) to pass the timeout + `self.fallback_shared()`. The hub needs the configured `direct_timeout`; add a `direct_timeout: std::time::Duration` field to `DeliveryHub` set at construction (default from config) and pass it here. Update all `DeliveryHub::new*` constructors to set it (non-fallback constructors set a default, e.g. `Duration::from_secs(20)`, since they pass `fallback: None` and the timer is inert).

- [ ] **Step 6: Pass `direct_timeout` from config in `state.rs`.** `config.direct_timeout_secs` (`daemon/config.rs:27`) → `Duration::from_secs(config.direct_timeout_secs as u64)`, handed to `new_with_inbound_dialer_and_fallback` (add the param). Default unchanged.

- [ ] **Step 7: Run the trigger test, verify it passes:**

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness sustained_direct_failure_triggers_fallback
```
Expected: PASS.

- [ ] **Step 8: Per-task gates.** Expected: green. (The `clippy::too_many_arguments` allow already sits on `full_run`; if `spawn` trips it, add `#[allow(clippy::too_many_arguments)]`.)

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/delivery/hub.rs crates/core/src/daemon/state.rs
git commit -m "feat(2.C): per-peer sustained-failure timer triggers direct->mailbox fallback (Task 20.5)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: End-to-end offline guardrail + final verification

**Why:** The exit criterion — prove, through the real `run_with_transport` assembly, that one peer being offline results in mailbox delivery the recipient receives on poll.

**Files:**
- Create: `crates/tests/src/offline_fallback.rs`
- Modify: `crates/tests/src/lib.rs` (add `mod offline_fallback;` — match how existing test modules are declared)
- Modify: `crates/core/src/lib.rs` `test_exports` (only if the test needs a not-yet-exported hook; prefer reusing `run_loopback` / `seed_established_pair`)
- Modify: `crates/tests/src/loopback_harness.rs` (helper to send while a peer is offline, if needed)

- [ ] **Step 1: Write the end-to-end guardrail test.** In `crates/tests/src/offline_fallback.rs` (license header), build on the existing loopback guardrails (`daemon_run_direct.rs` for the two-daemon `run_with_transport` setup, `mailbox_offline_delivery.rs` for the mailbox server + deposit/fetch shape):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 2.C guardrail: peer offline -> direct fails -> timeout -> retarget ->
//! sweeper deposits to mailbox -> recipient polls -> receives & decrypts.

#[tokio::test]
async fn offline_peer_receives_via_mailbox_fallback() {
    // 1. Alice + Bob daemons via run_with_transport over LoopbackTransport,
    //    established MLS pair (seed_established_pair), Bob advertises a mailbox.
    // 2. Spin an in-process mailbox server reachable by the loopback factory.
    // 3. Make Bob unreachable directly (don't run Bob's accept side / drop his
    //    inbound route) so Alice's direct delivery fails.
    // 4. Alice sends. Direct fails for direct_timeout (compressed via config) ->
    //    actor triggers fallback -> sweeper deposits to the mailbox.
    // 5. Bob's poll scheduler fetches the deposit -> dispatch_mailbox persists ->
    //    Event::MessageReceived. Assert Bob receives Alice's plaintext.
}
```

> NOTE TO IMPLEMENTER: read `daemon_run_direct.rs`, `mailbox_offline_delivery.rs`, and `loopback_harness.rs` in full before writing. Compress timing via `config_for` (set `direct_timeout_secs` low) and, if the 15 s sweep interval makes the test slow, expose the sweep interval through a test-only override (env var read in `state.rs` gated on `cfg!(feature = "test-harness")`, or a `test_exports` hook) — prefer the smallest change that keeps the test under ~30 s. Document any timing knob in the test.

- [ ] **Step 2: Run it, verify it fails or is red for the right reason first** (e.g. before wiring it asserts nothing arrives), then passes once the scenario is correct:

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-tests offline_peer_receives_via_mailbox_fallback -- --test-threads=1 --nocapture
```
Expected: PASS (Bob receives Alice's message via the mailbox).

- [ ] **Step 3: Add the targeted unit tests** if not already covered by Tasks 1–6:
  - ts-poison: a > 1h-old deposit surfaces exactly once via `dispatch_mailbox` and the second dispatch is a dedup no-op (extend `inbound.rs` `dispatch_mailbox` tests, reusing the trial-decrypt test at `inbound.rs:1278`).

```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness dispatch_mailbox
```
Expected: PASS.

- [ ] **Step 4: FULL final gate** (CI-parity, single-threaded authoritative):

```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
```
Expected: fmt clean; clippy clean; core all green; `skattr-tests` all non-ignored green including `offline_peer_receives_via_mailbox_fallback`, both pre-existing loopback guardrails, and `remove_mailbox` drain.

- [ ] **Step 5: Verify the CLI still builds** (no `skattr-ui`):

```bash
. "$HOME/.cargo/env" && cargo build -p skattr-cli
```
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/tests/src/offline_fallback.rs crates/tests/src/lib.rs crates/tests/src/loopback_harness.rs crates/core/src/lib.rs crates/core/src/daemon/inbound.rs
git commit -m "test(2.C): end-to-end offline-fallback guardrail + ts-poison unit coverage

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review (against the spec)

**Spec coverage:**
- §1 outbox carries kind → Task 1 ✓
- §2 timeout trigger → Task 6 ✓
- §3 sweeper → Task 5 ✓
- §4 hub fallback wiring → Task 4 ✓
- §5 RemoveMailbox drain → Task 3 ✓
- §6 ts-window exemption → Task 2 ✓
- §7 guardrail + targeted tests → Task 7 ✓
- Exit criteria 1–6 → Tasks 4/5 (production fallback + sweeper spawned), 6+5 (retarget+deposit), 3 (drain), 2 (poison), 7 (guardrail), 7 Step 4 (gates) ✓

**Type consistency:** `MailboxFallbackShared` (Task 4) is the type referenced by Tasks 5 (`run_mailbox_sweep`, `redeposit_mailbox_row`) and 6 (`full_run` param, `run_mailbox_fallback`). `OutboxEntry.target_kind`/`mailbox_id` (Task 1) are consumed by Task 6's filter. `enforce_ts_window` (Task 2) is the param name used in `receive`/`receive_in_tx`/`dispatch_for_group`. `OutboxRepo::due_mailbox` (Task 5) is the only new query. `now_unix_millis` (Task 5) is the clock used by both the sweeper and consistent with the outbox's millis `next_retry_at`. Consistent across tasks.

**Placeholder scan:** No "TBD"/"implement later". Test bodies that depend on existing scaffolding carry explicit IMPLEMENTER notes pointing at the exact files/lines to copy from, rather than inventing APIs — this is deliberate for tests that must reuse the codebase's bespoke handshake/mailbox-server fixtures.

**Security invariants preserved:** mailbox path replay resistance = `(sender, envelope_id)` dedup + MLS generation + server delete (Task 2 note); no secrets logged; deposits use the depositor-anonymous `Deposit` frame (no wire change). Wire-format neutral throughout.
