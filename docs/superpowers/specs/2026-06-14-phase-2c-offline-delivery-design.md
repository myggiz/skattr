# Phase 2.C — Offline Delivery: Fallback + Drain (Design)

**Date:** 2026-06-14
**Status:** Approved (brainstorming complete); plan to follow.
**Predecessor:** Phase 2.A (MLS ratchet & binding integrity) merged 2026-06-14.
**Source:** `docs/superpowers/specs/2026-06-13-phase-2-decomposition.md` §2.C;
audit items T1-6, T1-4, and the 1A ts-replay-poison TODO.
**Soft dependency:** 2.A (offline deliveries ride a sound ratchet) — satisfied.

This sub-project closes the deferred half of Phase 1's offline-delivery
guardrail: a message to an offline peer must automatically fall back to a
semi-trusted mailbox, retry until deposited, and surface on the recipient's
next poll — including legitimately-delayed deposits — and removing a mailbox
must preserve the messages it still holds.

**Wire-format / protocol neutral.** No new `Command` / `CommandResult` /
`Event` variants; no `Frame` changes; the mailbox wire protocol (ADR 0006)
is untouched. All work is internal delivery plumbing. No ADR required.

---

## Ground truth (verified against code 2026-06-14)

- **T1-6 fallback is unwired.** `DeliveryHub::ensure_mailbox_fallback`
  (`delivery/hub.rs:374`) exists and is unit-tested, but `Daemon::run`
  builds the hub via `new_with_inbound_and_dialer` (`daemon/state.rs:322`),
  which sets `fallback: None`. The per-peer direct-timeout trigger
  (Task 20.5) is an unwired TODO at `delivery/peer.rs:283`.
- **Outbox routing kind is silently dropped.** `OutboxEntry` and
  `row_to_entry` (`delivery/outbox.rs:21,88`) carry neither `target_kind`
  nor `mailbox_id`, even though the storage row (`storage/outbox.rs:50`)
  has both. The per-peer retry tick (`delivery/peer.rs:367`) reads
  `Outbox::due()` and sends every due row as a direct `Frame::MlsApp` to
  the peer — so a `target_kind='mailbox'` row is mis-sent over the direct
  path. No periodic loop deposits mailbox-kind rows.
- **T1-4 RemoveMailbox drain destroys held messages.** `handle_remove_mailbox`
  (`daemon/dispatch.rs:1244`) runs a final `run_one_poll_tick` (fetch +
  server-side delete) but `let _ =`-discards the returned `FetchResponse`
  (Task 22.5, `dispatch.rs:1281`). The `dispatch_mailbox` trait method
  already exists (`daemon/inbound.rs:568`) and can be reused.
- **ts-replay poison.** `dispatch_mailbox_inner` (`daemon/inbound.rs:307`)
  trial-decrypts, then persists through `dispatch_for_group` →
  `receiver::receive_in_tx`, which rejects any envelope whose `ts` is
  outside ±1h (`delivery/receiver.rs:74`). A legitimately-old
  store-and-forward deposit is `Rejected`, never ACKed/deleted, and
  re-fetched every poll (poison) — the delayed message never surfaces.
  Dedup is independent of the window: `receive_in_tx` calls
  `seen.insert_in_tx(tx, &sender.0, &envelope.id.0, now_ms)`
  (`receiver.rs:80`) keyed on `(sender, envelope_id)`.

## Locked decisions (brainstorming, 2026-06-14)

1. **Fallback trigger = sustained timeout.** Fire after `direct_timeout_secs`
   of unbroken direct-delivery failure for a peer (config already exists,
   range 1..=600 at `daemon/config.rs`), not on the first dial blip.
2. **Deposit retry loop = dedicated mailbox-outbox sweeper.** A new
   background task symmetric with the inbound poll scheduler. The per-peer
   direct actor stays strictly direct-only.
3. **ts-poison fix = exempt the mailbox path from the ±1h window.**
   Store-and-forward deposits are legitimately hours/days old. Replay
   resistance on the mailbox path comes from `(sender, envelope_id)` dedup
   + MLS generation monotonicity + server-side delete-after-fetch. The
   direct path keeps the ±1h check unchanged.

---

## Architecture

Seven units, grouped into three independent leaves and one spine.

```
leaf:  §1 outbox-kind ──┐
leaf:  §6 ts-exemption  │
                        ├─► §7 guardrail (end-to-end)
spine: §2 trigger ─► §3 sweeper ─► §4 hub wiring ─┘
leaf:  §5 RemoveMailbox drain
```

### §1 — Outbox carries routing kind

The corruption fix. Mailbox-kind rows must never be retried over the direct
path.

- Add `target_kind: OutboxTargetKind` and `mailbox_id: i64` to `OutboxEntry`
  (`delivery/outbox.rs`). `row_to_entry` copies both from `OutboxRow`.
- The per-peer direct retry tick (`delivery/peer.rs:367`) skips any due
  entry where `target_kind == OutboxTargetKind::Mailbox` (alongside the
  existing `entry.target != peer` and `pending.contains_key` guards).
- `Outbox::due` keeps returning all due rows; callers filter by kind. (The
  sweeper, §3, consumes the mailbox-kind rows.)

*What it does:* enriches the in-memory outbox entry so each consumer routes
by kind. *Depends on:* `storage::outbox::OutboxRow` (already carries both
fields). *Files:* `delivery/outbox.rs`, `delivery/peer.rs`.

### §2 — Direct→mailbox timeout trigger (Task 20.5)

- In `full_run` (`delivery/peer.rs`), track a `first_failure_at:
  Option<Instant>`: set on the first direct send/dial failure to this peer
  while rows are pending, cleared on any successful send.
- A new tick (or reuse the 1 s retry tick's cadence) checks whether
  `first_failure_at.elapsed() >= direct_timeout`. On expiry, **retarget**
  this peer's due `target_kind='direct'` rows to mailbox-kind: for each, pick
  a mailbox deterministically by `BLAKE2s(message_id)` over the recipient's
  advertised mailbox list and call `OutboxRepo::set_mailbox_target`. The row
  stays in the outbox, now mailbox-kind and due. Retargeting only — no
  deposit here.
- The selection logic (pick-one + the recipient's mailbox list) is the same
  used by `ensure_mailbox_fallback`; it is factored into a shared helper so
  the trigger and the sweeper agree on which mailbox a message routes to.
- The trigger reaches the recipient's mailbox list + `OutboxRepo` through a
  fallback-trigger handle injected into `full_run`, mirroring how `dialer`
  is injected today (`Option<Arc<...>>`, `None` in direct-only tests). The
  handle is owned by the hub's `MailboxFallback`.
- `direct_timeout` is read from config at hub construction and passed into
  the actor; default unchanged.

*What it does:* converts sustained direct-delivery failure into a routing
decision. *Depends on:* §1 (kind on the entry), the hub's `MailboxFallback`.
*Files:* `delivery/peer.rs`, `delivery/hub.rs`.

### §3 — Mailbox-outbox sweeper (deposit + retry engine)

A new module `delivery/mailbox_sweeper.rs` and a task spawned in
`Daemon::run`.

- Each tick (a fixed interval, e.g. the existing poll cadence): read due
  `target_kind='mailbox'` rows (`Outbox::due` filtered by kind, bounded
  batch).
- For each row: deposit the payload to the selected mailbox via the
  `MailboxConnectFactory`, walking the recipient's advertised mailbox list
  on failure (sequential failover). On a successful deposit, `delete_by_id`
  the row. On failure across all candidate mailboxes, `reschedule` with
  backoff (reuse `delivery::backoff`).
- The per-row "select mailbox → deposit → delete/reschedule" core is the
  refactor of `ensure_mailbox_fallback`'s body into a reusable `deposit_one`
  on the hub (or sweeper). `ensure_mailbox_fallback`'s existing public
  signature (`peer, message_id, ciphertext`) is preserved as a thin wrapper
  — it finds/retargets the direct row, then calls `deposit_one` — so the
  existing `mailbox_offline_delivery` / `mailbox_failover` tests stay green.
- The sweeper task is stored on the daemon's task set and aborted on
  shutdown (mirrors the hub's `sweep` JoinHandle).

*What it does:* the single code path that turns mailbox-kind outbox rows
into mailbox deposits, with retry + failover. *Depends on:* §1, §4 (factory
on the hub). *Files:* new `delivery/mailbox_sweeper.rs`, `delivery/hub.rs`,
`daemon/state.rs`.

### §4 — Wire fallback into the production hub

- `ensure_mailbox_fallback` and `deposit_one` require the hub's
  `MailboxFallback` to be `Some`. Today's production constructor
  (`new_with_inbound_and_dialer`) sets it to `None` while the
  fallback-capable constructor (`new_with_mailbox_fallback`) sets `dialer`
  to `None` — they are mutually exclusive.
- Add one constructor that carries **both** the on-demand `dialer` and the
  `MailboxFallback` (factory + events + identity). `Daemon::run`
  (`daemon/state.rs:322`) switches to it, passing `Some(MailboxFallback{..})`
  built from the `mailbox_factory` already in scope.

*What it does:* makes fallback reachable in production without losing
dial-on-demand. *Depends on:* nothing new. *Files:* `delivery/hub.rs`,
`daemon/state.rs`.

### §5 — RemoveMailbox drain dispatches held messages (Task 22.5)

- Thread an `Option<Arc<dyn InboundDispatch>>` onto `DaemonHandle` (the
  handle already shares `group_locks` with the inbound dispatcher; this
  field is set the same way in `Daemon::run`). `None` in tests that don't
  exercise inbound.
- In `handle_remove_mailbox` (`daemon/dispatch.rs:1278`), capture the
  `FetchResponse` from the final `run_one_poll_tick` instead of discarding
  it. For each `PendingDeposit`, call `inbound.dispatch_mailbox(&ciphertext)`
  before `finalize_removal`. Trial-decrypt + persist + emit
  `Event::MessageReceived` is handled inside `dispatch_mailbox` /
  `dispatch_mailbox_inner` (already exists).
- Remains best-effort: an unreachable mailbox, an absent inbound ref, or a
  non-decrypting deposit must not block removal.

*What it does:* drains held offline messages into storage before the
mailbox is forgotten. *Depends on:* §6 (a drained deposit is typically
delayed, so it must also be exempt from the ts-window). *Files:*
`daemon/dispatch.rs`, `daemon/handle.rs`, `daemon/state.rs`.

### §6 — ts-window exemption for the mailbox path (poison fix)

- Add an `enforce_ts_window: bool` parameter to `receiver::receive` and
  `receiver::receive_in_tx`. When `false`, skip the ±1h check at
  `receiver.rs:74`; the `seen.insert_in_tx` dedup and message insert run
  unchanged.
- Thread the flag from `DaemonInbound::dispatch_for_group`: the direct path
  (`dispatch`) passes `true`; the mailbox path (`dispatch_mailbox_inner` →
  `dispatch_for_group`) passes `false`.
- Concretely, `dispatch_for_group` gains the flag (or a small
  `DeliverySource { Direct, Mailbox }` enum) so it can choose; the
  ts-poison TODO comment at `inbound.rs:301` is removed.

*What it does:* lets legitimately-old offline deposits land while keeping
replay resistance for the live direct path. *Depends on:* nothing.
*Files:* `delivery/receiver.rs`, `daemon/inbound.rs`.

### §7 — Guardrail + targeted tests (exit criterion)

- **End-to-end loopback guardrail** (`crates/tests/src/`, extending
  `loopback_harness.rs`): Alice and Bob established (seeded pair); Bob is
  offline (no live direct route); Alice sends; her direct delivery fails;
  after the timeout the row is retargeted; the sweeper deposits to Bob's
  mailbox; Bob polls; Bob receives and decrypts. Through the real
  `run_with_transport` assembly (not hand-wired). Compress `direct_timeout`
  + sweeper interval for the test.
- **ts-poison unit test:** a mailbox deposit with `ts` > 1h old surfaces
  exactly once via `dispatch_mailbox` and is not re-poisoned on a second
  dispatch (dedup catches the replay).
- **RemoveMailbox drain test** (extend `remove_mailbox_drains.rs`): a held
  deposit on the removed mailbox is dispatched into storage before
  finalization.
- **Outbox-kind regression:** a `target_kind='mailbox'` due row is skipped
  by the direct retry tick and consumed by the sweeper.

*Files:* `crates/tests/src/` (new + extended), `loopback_harness.rs`.

---

## Error handling

- Deposits, drains, and triggers are **best-effort**: a mailbox that is
  unreachable, returns an error, or times out leaves the outbox row in place
  for the sweeper's next tick (rescheduled with backoff) and never blocks
  the daemon, a send, or a mailbox removal.
- `reschedule` uses the existing `delivery::backoff` (capped exponential
  with jitter) — no unbounded retry storms.
- All new code obeys the workspace lints: no `unwrap`/`expect` in non-test
  paths (`?` + typed `CoreError`); guards dropped before `.await`
  (`await_holding_lock`); never log onions/pubkeys/ciphertext at `info`+.

## Security notes

- **Replay resistance on the exempted path.** Removing the ±1h window for
  mailbox deliveries does not weaken replay protection: `(sender,
  envelope_id)` dedup (`seen_messages`, enforced in the same transaction as
  the message insert) rejects any duplicate, MLS generation numbers give
  authoritative ordering, and the mailbox deletes each deposit after a
  successful fetch. A replayed-by-the-mailbox ciphertext is caught by dedup;
  a forged ciphertext fails MLS decryption.
- **Metadata exposure is unchanged.** Fallback deposits use the existing
  depositor-anonymous `Deposit` frame (ADR 0006); the sweeper introduces no
  new identifiers on the wire. Sustained-timeout (not immediate) fallback
  minimizes how readily traffic shifts onto the semi-trusted mailbox.

## Out of scope (unchanged deferrals)

- Mailbox-server resource hardening (T1-5) and the accept-loop spawn bound
  — Phase 2.D.
- At-rest DB encryption (T1-2) — Phase 2.B.
- Real onion-key rotation (Task 23.5), multi-member groups,
  metadata-minimization — v1.1+.
- Mailbox fallback for Welcome propagation (Task 2.E.5) — would touch the
  ADR 0006 freeze; remains deferred.

## Exit criteria

1. The production hub is built with fallback enabled; `Daemon::run` spawns
   the sweeper.
2. Direct delivery failing for `direct_timeout_secs` retargets the peer's
   pending rows to a mailbox; the sweeper deposits them with failover and
   backoff; mailbox-kind rows are never retried over the direct path.
3. Removing a mailbox dispatches its still-held deposits into storage before
   finalizing.
4. A legitimately-delayed (> 1h) mailbox deposit surfaces exactly once; no
   poison deposit; the direct path keeps the ±1h replay window.
5. The end-to-end offline guardrail (one peer offline → mailbox → poll →
   receive, through `run_with_transport`) passes, plus the targeted unit
   tests.
6. `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui
   --all-targets --features test-harness -- -D warnings`, core test suite,
   and `skattr-tests` (single-threaded) are all green.

## Delivery model

`spec (this doc) → writing-plans → subagent-driven execution →
two-stage review per task → verification → finish branch`.
