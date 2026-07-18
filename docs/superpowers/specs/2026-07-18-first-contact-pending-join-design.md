# First-contact state divergence fix — `PendingJoin` + durable Welcome re-send

**Date:** 2026-07-18
**Issue:** #93 (Mode B of #90). Relates: #90 (Mode A — Arti Conflux transport
teardown), f_review finding #4 (dead `PendingJoin` state).
**Area:** MLS group lifecycle + first-contact delivery — protocol/auth-adjacent.
**Requires a second reviewer** (per CLAUDE.md). Decide during planning whether an
ADR note is needed (first-contact state lifecycle; no wire-format change).

---

## Problem

On first contact the **invitee** (the side that consumes an invite via
`add_contact`) builds the two-PSK genesis commit and **immediately finalizes its
MLS group as `GroupState::Active`** — before the **responder** (inviter) has
Ack'd the `Welcome` and actually joined.

- `crates/core/src/mls/group.rs` sets `GroupState::Active { .. }` in all three
  commit paths (`:175`, `:349`, `:426`).
- `GroupState::PendingJoin` (`state_machine.rs:23`) is never constructed — dead
  state (f_review #4).
- The Welcome is delivered by an **in-memory** `tokio::spawn` retry loop
  (`dispatch.rs:459–498`, ~6 min of backoff) that then **gives up**; there is no
  durable re-send. The code already documents this gap (`dispatch.rs:433–439`).

**Failure (confirmed two-machine, both on `0dd14f9`):** if that first Welcome
fails to complete (in the field via #90 Mode A — the HS circuit is torn down
before the Ack; but *any* transient failure does it), the invitee still believes
it is paired (group is `Active`, `can_send()==true`). On every subsequent
dial/retry it therefore sends **`MlsApp`** app frames, not a `Welcome`. The
responder is an unknown peer (never joined) whose ADR-0007 carve-out only accepts
a `Welcome` from an unknown peer, so it rejects every `MlsApp`
(`accept: rejected unknown peer — first frame was not a Welcome frame=MlsApp`).
**Permanent stuck state, no recovery** — first-contact Welcome is direct-only, and
nothing re-issues a Welcome once the invitee has locally committed.

This is a "green tests, dead production path" bug: the loopback guardrail
`first_contact_invite_add_then_bidirectional_over_loopback` passes because
loopback never drops the first Welcome, so the retry-with-`MlsApp` path is never
exercised.

## Goals / non-goals

**Goals**
- The invitee does not consider itself paired until the responder joins (Ack).
- A first Welcome that fails leaves a **recoverable** invitee that re-sends
  **Welcomes** (never app frames) until first contact completes.
- Recovery is **durable** — survives a daemon restart.
- App sends are **blocked** while pending, with a clear user-facing reason.
- The whole lifecycle is **observable** (redaction-safe) and the state is
  **inspectable** (CLI `contacts` group_state + `pending_welcomes`).
- Close the test gap with a **non-loopback** first-contact test that drops the
  first Welcome.

**Non-goals**
- Mode A (Arti Conflux/multi-path circuit teardown) — stays #90.
- **Mailbox fallback for the first-contact Welcome** — remains the known
  direct-only limitation (touching it extends the frozen ADR 0006). This fix
  makes the *direct* re-send durable; it does not add a mailbox path.
- Queuing user messages typed while pending — out of scope; sends are blocked
  (see below).

## Roles

The fix is **entirely on the invitee / committer side**. The responder
(accept-loop → join → Ack, `accept.rs`) is already correct and unchanged; its
join is idempotent, so a duplicate re-sent Welcome after a successful join is
safe.

## Design

### 1. Reuse the dead `PendingJoin` state

The invitee's group enters `GroupState::PendingJoin` at the genesis commit
instead of `Active`, and transitions to `Active { epoch }` only when the
responder's Welcome **Ack** arrives.

- `can_send()` is already `true` only for `Active`, and `group.encrypt()` checks
  `can_send()`, so this **automatically blocks `MlsApp`** while pending — no
  separate send gate needed. This is what eliminates the stray `MlsApp` by
  construction.
- Broaden the `PendingJoin` doc-comment to cover both senses: "joiner holds an
  unprocessed Welcome" *and* "committer has committed the genesis but the peer
  has not joined yet."
- `can_receive()` while `PendingJoin` stays `false`; this is safe because the
  peer cannot send anything until it joins, and joining triggers the Ack that
  flips us to `Active` (→ `can_receive()==true`) before any inbound app frame.

### 2. Persist the Welcome (durable re-send)

Add a durable table **`pending_welcomes`** (new migration), mirroring the
`attachment_deposits` / `outstanding_invites` patterns:

| column | type | notes |
|---|---|---|
| `peer_pubkey` | BLOB (32) | the responder's identity pubkey; PK |
| `group_id` | BLOB | the genesis group id |
| `welcome_bytes` | BLOB | the exact `Welcome` message to re-send |
| `next_retry_at` | INTEGER | ms; due-time for the next send |
| `attempts` | INTEGER | for backoff + diagnostics |
| `created_at` | INTEGER | ms |

`AddContact` writes this row **in the same transaction** that saves the genesis
group (`dispatch.rs:418`), so persistence is atomic with the commit.

A **`welcome_sweeper`** task (sibling to `delivery::chunk_sweep` /
`mailbox_sweeper`, spawned in `run_with_transport`) is the **sole** Welcome
delivery path: it sends due pending Welcomes (`next_retry_at <= now`) with
bounded backoff (interval caps ~60s, attempts unbounded), awaits the Ack, and on
success runs the transition in §3. It re-queries `pending_welcomes` on boot, so
it survives restart. `add_contact` persists the row with `next_retry_at = now`
and **nudges** the sweeper (a `tokio::sync::Notify`) so the first send is prompt
without a separate code path. This **removes** the in-memory `tokio::spawn` retry
that gives up (`dispatch.rs:459–498`) — there is exactly one place that sends and
handles the Welcome, which avoids a dual-path race.

### 3. Ack → `Active` transition (idempotent CAS)

The Welcome-Ack handling (where `welcome: acked by peer` is logged today,
`dispatch.rs:472`, moving into the sweeper) now atomically and idempotently, on
each successful Ack:

1. loads the group,
2. **compare-and-set** `PendingJoin → Active { epoch }` (if already `Active`,
   no-op — handles the immediate/sweeper double-Ack race),
3. persists the group,
4. **deletes** the `pending_welcomes` row,
5. sends the self-card (existing behavior),
6. emits `Event::ContactUpdated` so the UI leaves "Connecting…".

Never flip to `Active` without a confirmed Ack. If load/save fails mid-transition,
log and leave `PendingJoin`; the sweeper re-sends and the peer re-Acks
(idempotent) — eventually consistent.

### 4. Block sends while pending (free, + clean error)

While `PendingJoin`, `group.encrypt()` rejects, so `send_message`
(`dispatch.rs:517`) fails. Map that specific state to a clear
`DaemonErrorKind` → user-facing *"not connected yet — waiting for them to
join"* instead of a raw MLS error. The UI already renders a `pending_join`
"Connecting…" affordance (CLAUDE.md 4.C).

### 5. Cancel

`RemoveContact` / archive also deletes any `pending_welcomes` row for that peer,
so removing a stuck pending contact stops the re-send. (No silent give-up
otherwise.)

## Observability (debug at every step) — redaction-safe, no peer/onion logged

- `add_contact`: `first-contact: group committed PendingJoin; welcome persisted for durable re-send`
- sweeper: `welcome-sweeper: {n} pending welcomes due`, `welcome-sweeper: re-sending pending welcome attempt={n}`, on boot `welcome-sweeper: resumed {n} pending welcomes from durable state`
- Ack: `welcome: acked — group PendingJoin→Active` (+ the dialer-side
  `debug: welcome frame written; awaiting Ack` and a drain-path `warn!` promised
  on #90)
- blocked send: `debug: send blocked — group still PendingJoin (peer not joined)`
- Complements the #90 instrumentation already merged (accept 4-way split + the
  three `peer.rs` welcome-send warns).

**Inspectable state:** CLI `contacts` already surfaces `group_state`
(`Active` vs `pending_join`). Additionally surface `pending_welcomes`
(`attempts` / `next_retry_at`) — via a debug/log path — so a stuck re-send is
visible without guessing.

## Error handling summary

| Situation | Behavior |
|---|---|
| First Welcome fails (Mode A or any transient) | group stays `PendingJoin`; durable row re-sends |
| Two overlapping re-sends both Ack'd (sweeper) | CAS `PendingJoin→Active` is a no-op the second time; row-delete idempotent |
| Duplicate re-sent Welcome after peer already joined | responder join is idempotent — safe |
| Group load/save fails during Ack transition | log, stay `PendingJoin`; sweeper retries; eventually consistent |
| Daemon restart mid-flight | sweeper re-queries `pending_welcomes`; group persisted `PendingJoin`; re-drive resumes |
| User sends while pending | `send_message` returns "not connected yet" (no `MlsApp`) |
| User removes the pending contact | `pending_welcomes` row deleted → re-send stops |

## Test plan

**Unit**
- `GroupState` `PendingJoin→Active` CAS, including double-Ack no-op.
- `pending_welcomes` repo: persist / `due(now)` / delete / idempotent delete.
- `send_message` while `PendingJoin` returns the clean "not connected yet" error
  and emits **no `MlsApp`**.
- `welcome_sweeper` re-drives due rows and reschedules with backoff.

**Integration — closes the #90/#93 gap**
A first-contact test over a **fault-injecting transport** that drops the *first*
`MlsWelcome` frame then delivers, asserting:
- the invitee stays `PendingJoin` after the drop,
- it sends **no `MlsApp`**,
- the sweeper re-sends a `Welcome`,
- first contact completes on a later attempt,
- **both sides go `Active`**.

This reproduces Mode B **without** real-Tor flakiness (pure state divergence);
the existing loopback guardrail cannot catch it because it never drops the
Welcome. Implement the drop via a transport wrapper over the loopback harness
that fails the Nth `MlsWelcome` (no new network dependency).

## Migration

New migration `0017_pending_welcomes` (next after `0016`). Additive; no change to
existing tables beyond the new one. Schema-version bump handled by the existing
migration runner (with the `SchemaTooNew` downgrade guard).

## Files (anticipated)

- `crates/core/src/mls/group.rs` — genesis commit sets `PendingJoin`; add the
  `PendingJoin→Active` CAS helper.
- `crates/core/src/mls/state_machine.rs` — broaden `PendingJoin` doc.
- `crates/core/src/storage/pending_welcomes.rs` (new) + migration
  `0017_pending_welcomes.sql`.
- `crates/core/src/delivery/welcome_sweep.rs` (new) — the sweeper.
- `crates/core/src/daemon/dispatch.rs` — persist row in the add_contact txn;
  Ack-path CAS + row delete; `send_message` clean-error mapping; wire cancel into
  `RemoveContact`.
- `crates/core/src/daemon/state.rs` — spawn `welcome_sweeper` in
  `run_with_transport`; drain on shutdown.
- Tests as above (unit + the fault-injecting integration guardrail).
