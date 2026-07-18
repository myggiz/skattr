# First-contact recovery redesign — responder idempotent re-Ack

**Date:** 2026-07-18
**Issue:** #93 (Mode B of #90). Supersedes the recovery model in
`2026-07-18-first-contact-pending-join-design.md`.
**Area:** MLS group lifecycle + first-contact delivery — **protocol/auth-adjacent.**
**Requires ADR 0011 + the crypto/second reviewer** (per CLAUDE.md).

---

## Why this supersedes the earlier #93 recovery model

The earlier #93 design (durable `pending_welcomes` row + a sweeper that re-sends
the stored `Welcome` bytes) was implemented as Tasks 1–6. Task 7 — the
**non-loopback fault-injecting guardrail** the audit mandates — then exposed that
the recovery mechanism does **not** actually recover. This is the audit's own
"green tests, dead production path" class: the loopback guardrail passes only
because it never drops a frame, so the sweeper's re-send never fires.

Two independent facts break "re-send the stored Welcome":

1. **The invite is consumed on join.** The responder's first-contact join
   deletes the `outstanding_invites` row (`inbound.rs:584`,
   `mark_consumed_in_tx`). A re-sent Welcome then fails the `kp_ref` lookup
   (`inbound.rs:471-473`, `"inbound welcome: unknown kp_ref"`) → `dispatch_welcome`
   returns `None` → **no Ack** (`inbound.rs:670`).
2. **`h_transport` is per-connection.** `h_transport =
   HKDF(noise_handshake_hash, "skattr-binding-v1")` (ADR 0009) binds the MLS
   genesis to **one** Noise session. The sweeper re-sends over a **new** dial →
   new handshake hash → different `h_transport` → the stored Welcome's baked-in
   PSK no longer matches → join fails.

So the design's §Roles claim — "the responder join is idempotent, so a duplicate
re-sent Welcome after a successful join is safe" — is **false**: the duplicate is
rejected *before* reaching any idempotent-join logic.

### The two failure cases (precise)

- **Lost-Ack** — responder received the Welcome, **joined, consumed the invite**,
  sent an Ack that was lost. This is #90 Mode A ("the HS circuit is torn down
  before the Ack") — **the actual observed field bug.**
- **Lost-Welcome** — responder never received the Welcome, never joined, invite
  still valid.

## Goals / non-goals

**Goals**
- **Lost-Ack recovers** through the real `run_with_transport` assembly: a lost
  first-contact Ack self-heals without user action and without the invitee ever
  emitting an app frame while pending.
- The invitee side (Tasks 1–6) is **preserved**, not rewritten.
- The recovery behavior is **observable** and proven by a fault-injecting
  guardrail that drops the Ack (the faithful lost-Ack reproduction).

**Non-goals**
- **Lost-Welcome recovery over a fresh circuit** — a re-sent Welcome carries the
  original connection's `h_transport`, so a responder that never joined cannot
  validate it on a new connection. **Disclosed as a v1.0 limitation** tied to #90
  Mode A. This is **not a regression**: a truly-lost first Welcome already
  stalled before #93 (the pre-existing "first-contact Welcome is direct-only"
  limitation).
- **Rebuild-per-dial / re-deriving `h_transport`** (the "Anchor 2" option) — the
  only way to also recover lost-Welcome, but it re-opens the ADR-0009 binding and
  overlaps the #90 transport work. Deferred to v1.1.
- **New wire frame / ADR-0006 change** — the fix reuses the existing
  `Frame::MlsWelcome` and `Frame::Ack`. No wire-format change.
- **A give-up / failure-surfacing timer for the futile lost-Welcome retry** — see
  §Futile retry. v1.1 polish.

## Design

### The one change: responder idempotent re-Ack

In the responder's first-contact Welcome path, **before** attempting the MLS
join, resolve the authenticated peer's identity (the Noise static key →
Ed25519, already resolved by the accept loop) and check whether that peer is
**already an established contact with a group**. If yes:

- return `Some(welcome_msg_id(welcome))` — the accept loop sends
  `Frame::Ack(id)`, resolving the invitee's `send_welcome` oneshot — **without**
  feeding the Welcome into MLS: no `welcome_join_persist`, no PSK/`h_transport`
  check, no group state mutation.

If no (peer not yet a member): proceed with the normal first-contact join
exactly as today (`dispatch_welcome_bootstrap` with the invite + `h_transport`).

Because the invitee re-sends the **identical** stored Welcome bytes,
`welcome_msg_id(welcome)` is identical to the original, so the re-Ack correlates
to the invitee's pending Welcome job.

### Membership check

The check is "is the authenticated peer already a contact with a non-null
`group_id`?" (first contact is 1:1 / 2-member, so one group per peer). Matching
on the peer identity is sufficient; if the Welcome's group id is cheaply
available it may be asserted as defense-in-depth, but the peer-identity match is
the authority.

### Why it is safe (auth reasoning for ADR 0011)

- The peer is **Noise-authenticated** on the connection — an attacker cannot
  provoke a re-Ack addressed to a different identity.
- The re-Ack path **does not process the Welcome's MLS content**, so a malicious,
  stale, or replayed duplicate Welcome cannot mutate or corrupt the existing
  group. It only re-emits an Ack the responder already legitimately earned by
  joining.
- No secret material is derived or exposed on the re-Ack path.

### Data flow (lost-Ack, end to end)

1. Invitee `add_contact` → builds Welcome, persists `pending_welcomes` row
   (`PendingJoin`), nudges the sweeper (Tasks 1–5).
2. Sweeper dials, sends Welcome (connection-1). Responder joins, consumes invite,
   sends Ack — **Ack lost** (circuit dies).
3. Invitee's `send_welcome` oneshot resolves `Err`/times out → sweeper reschedules
   with backoff (Task 4).
4. Sweeper re-dials (connection-2), re-sends the identical Welcome. Responder
   resolves the authenticated peer → already a contact with a group →
   **re-Acks** without re-joining.
5. Invitee's oneshot resolves `Ok(())` → `on_welcome_acked` deletes the
   `pending_welcomes` row (Task 4) → sends unblock → both sides `Active`.

### Futile retry (lost-Welcome), per decision

For lost-Welcome the sweeper keeps re-trying (capped 60 s backoff, one cheap dial
per interval) and never succeeds. **v1.0 keeps retrying** and relies on the
existing `pending_join` "Connecting…" UI badge; a give-up/timeout with a
user-visible failure event is a **v1.1** polish. Documented in the v1.0
limitations.

## Error handling

| Situation | Behavior |
|---|---|
| Lost-Ack (responder already joined) | responder re-Acks (no re-join); invitee clears pending; both `Active` |
| Lost-Welcome (responder never joined) | re-sent Welcome fails `h_transport` binding; invitee stays `PendingJoin`, keeps retrying; disclosed limitation |
| Duplicate Welcome after both already `Active` | re-Ack is idempotent; no MLS processing; harmless |
| Malicious/stale duplicate Welcome from the authenticated peer | re-Ack path never touches MLS state — cannot corrupt the group |
| Non-member peer sends an unknown Welcome | normal first-contact join path (unchanged); rejected if invite invalid |

## Test plan

**Unit (responder)**
- A Welcome from a peer already in a group with us returns the Ack id **without**
  invoking the MLS join (assert no group-state change; assert the returned id
  equals `welcome_msg_id(welcome)`).
- A Welcome from a non-member peer still takes the normal join path.

**Integration — closes the #90/#93 gap (retargeted Task 7)**
A first-contact test over the fault-injecting transport that **drops the first
Ack** (invitee→responder Welcome delivered; responder joins; the responder's Ack
is dropped once, connection kept faithful), asserting through the real
`run_with_transport` assembly:
- after the dropped Ack, the invitee is still **pending** (the `pending_welcomes`
  row exists / `group_state == pending_join`) and `send_message` returns the
  clean "not connected yet" error (**no app frame emitted while pending**);
- the sweeper re-sends; the responder **re-Acks** (idempotent, no re-join);
- the `pending_welcomes` row is deleted on the invitee; **both** daemons report
  `group_state == active`; a text message round-trips **both** directions.
- **Bug-catch check:** the test must FAIL without the responder re-Ack change
  (invitee never clears pending) — a guardrail that passes against the
  unfixed responder is worthless. Document the check in the commit.

Reuse the harness plumbing already written for Task 7
(`run_loopback_with_transport`, the `test_exports` re-exports, the fault-injecting
`Transport` wrapper); only the fault seam changes from first-frame-drop to
Ack-drop.

## ADR

**ADR 0011 — first-contact idempotent re-Ack.** Records: the responder re-Acks a
first-contact Welcome from a peer it is already grouped with, skipping MLS
processing; reuses `Frame::MlsWelcome`/`Frame::Ack` (no ADR-0006 wire change);
the auth reasoning above; and the disclosed lost-Welcome limitation. The task
carrying the code change requires the opus crypto/second reviewer.

## Files (anticipated)

- `crates/core/src/daemon/inbound.rs` — the membership short-circuit in the
  first-contact Welcome path (re-Ack without join); a helper to test whether an
  authenticated peer is already a grouped contact.
- `crates/core/src/daemon/accept.rs` — only if the routing of a now-known peer's
  Welcome needs the short-circuit wired at the accept-loop layer (verify during
  planning).
- `crates/tests/src/first_contact_welcome_dropped.rs` — retarget to drop the Ack;
  rename to reflect the lost-Ack scenario.
- `docs/adr/0011-first-contact-idempotent-reack.md` (new).
- Threat model / limitations docs — disclose lost-Welcome-over-new-circuit.

## What is preserved (no change)

Tasks 1–6, already committed: migration `0017_pending_welcomes` +
`PendingWelcomeRepo`; genesis `PendingJoin` + `set_active`; `send_message`
block-while-pending; `welcome_sweep` (durable re-send + `finalize_welcome_ack`
row-delete on Ack); `run_with_transport` sweeper wiring + nudge;
`RemoveContact` cancel.
