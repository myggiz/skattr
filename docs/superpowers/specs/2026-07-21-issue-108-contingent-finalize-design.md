# #108 — Contingent finalize via uniform pending-gate (design)

**Issue:** myggiz/skattr#108
**Milestone:** v1.1
**Status:** design approved (brainstorming 2026-07-21)
**Wire/protocol change:** none. **No ADR** (no wire, no MLS state-machine change). Crypto/protocol second-reviewer still required — this governs outbound frame emission on MLS groups.
**Branch:** `108-contingent-finalize`

## Goal

Enforce one invariant end-to-end: **while a first-contact Welcome is unacked
(`is_pending(peer)` is true), no outbound MLS application frame is emitted for
that peer — for any `Kind`.** A pending group is local-only until the peer joins
(Acks the Welcome).

This eliminates the `WrongGroupId` orphan-app-frame flood a never-joining peer
sees, and makes the invitee's local state honest at the transport level, not
just in the UI (#101) and not just for user text (#93).

## Motivation

Field-confirmed on v0.1.6 (2026-07-20, #90 Windows comment 17:34:36Z): a
responder that never joined still received MLS application frames it could only
drop —

```
WARN inbound: dispatch failed, dropping frame peer=PublicKey(…)
    err=mls: authentication failed: ValidationError(WrongGroupId)
```

**Root cause (investigation result).** #93 added a `pending_welcomes` send-guard,
but only on the user text path. There are exactly two production `Group::encrypt`
call sites in `crates/core/src/daemon/dispatch.rs`:

1. `send_message` (dispatch.rs:618) — **gated** by `is_pending` at dispatch.rs:594.
2. `send_card_to_contact` (dispatch.rs:1362) — **un-gated**.

`send_file` announces its `Kind::File` manifest through the `send_message` path,
so it inherits the guard. The **only** leak is `send_card_to_contact`, reached
by `publish_self_card_update` (dispatch.rs:1400 — broadcast the self-card to
every contact on `RotateOnion`, `RemoveMailbox` republish, and card updates).
When that broadcast runs while a first contact is still pending, it loads the
pending group, encrypts a `Kind::ContactCardUpdate` as `MlsApp`, and enqueues it
to a peer who never joined → `WrongGroupId` at the responder.

So #108 is not a missing "finalize" concept — the durable "not yet Active"
signal already exists (`is_pending`, #93; `GroupState` is not persisted,
`Group::load` always returns `Active`, so `is_pending` is the source of truth).
The defect is that this invariant is enforced at one emitter, not all of them.

## Decisions (locked in brainstorming)

1. **Uniform pending-gate, single shared predicate** — not a restructure of the
   two-PSK genesis / group creation (that deeper "defer group creation until
   Ack" option was rejected as high-risk and entangled with #107).
2. **Skip, don't queue-and-defer**, the card for a pending peer. The
   `welcome_sweep` already sends the reverse-direction self-card immediately
   after Ack, so a skipped pending-time card is delivered post-Ack anyway.

## Architecture / behavior

### The invariant and its single predicate

Add one helper as the sole source of truth for the guard:

```rust
/// A first-contact Welcome to `peer` is still unacked (#93/#108): the local
/// MLS group exists but MUST NOT emit outbound application frames yet.
fn is_peer_pending(pool: &Pool, peer: &PublicKey) -> Result<bool> {
    PendingWelcomeRepo::new(pool).is_pending(&peer.0)
}
```

(Exact module placement — a free fn in `dispatch.rs` near the emitters, or a
small method — is an implementation detail; the constraint is that both emitter
sites and the tests call the *same* predicate, no duplicated inline `is_pending`
logic.)

### Emitter sites

- **`send_card_to_contact` (the leak, dispatch.rs:1292):** at the top, before
  the `Group::load` → `encrypt` → enqueue sequence, check `is_peer_pending`. If
  pending, **skip** this peer — return early, best-effort, `debug!`-logged
  (matching the function's existing skip-on-encrypt-failure style; it already
  returns `()` and swallows non-fatal cases). No error propagates;
  `publish_self_card_update`'s broadcast loop simply cards the connected
  contacts and skips the pending ones.
- **`send_message` (dispatch.rs:594):** keep the guard; refactor it to call
  `is_peer_pending` so the predicate is shared. Behavior is unchanged — a
  pending send still returns
  `InvalidArgument { message: "not connected yet — waiting for them to join" }`.
- **`send_file`:** inherits the block via the `send_message` path (no separate
  `encrypt` site). A test locks this so a future refactor that gives `send_file`
  its own encrypt path can't silently bypass the gate.

### Post-Ack safety (why gating the card is safe)

`welcome_sweep::on_welcome_acked` runs `delete pending_welcomes row` **before**
`send_card_to_contact` (the reverse-direction self-card). So by the time that
call runs, `is_pending` is already false and the card sends normally. Gating
`send_card_to_contact` on `is_peer_pending` therefore never blocks the
legitimate post-Ack card. This ordering is load-bearing and will be asserted by
a test so a future reorder can't regress it.

## Error handling

- The card skip is best-effort and non-fatal (it mirrors the existing
  `card-send: encrypt failed; skipping` path): log at `debug!`, return, do not
  error. A skipped pending peer is a normal transient state, not a failure.
- `is_peer_pending` returning `Err` (a real storage error) is propagated where
  the caller already handles `Result` (`send_message`), and in
  `send_card_to_contact` it is logged and treated as skip (the function is
  already infallible to its callers; a storage error there must not abort a
  whole broadcast — but it MUST NOT send either, so on `Err` skip and `warn!`).

## Testing

- **Guardrail — no app frames to a never-joining peer:** with a pending contact
  (post-`add_contact`, no Ack) and a second connected contact, invoke
  `publish_self_card_update`. Assert: **zero** outbox rows / frames for the
  pending peer (and no `Group::encrypt` on its group), and the connected contact
  still receives its `ContactCardUpdate`. This is the acceptance criterion
  ("zero `WrongGroupId` at the responder") proven at the emission point.
- **`send_file` while pending is blocked:** `SendFile` to a pending contact is
  rejected/emits nothing, same as `send_message` — locks the shared-path
  assumption.
- **Post-Ack card sends:** after the pending row is cleared (Ack simulated),
  `send_card_to_contact` to that peer emits the card (proves the gate doesn't
  over-block).
- **Ordering guard:** a focused test on `on_welcome_acked` asserting the pending
  row is deleted before the self-card send (so gating the card is safe).
- Prefer a live `run_with_transport` guardrail if a self-card broadcast can be
  driven against a never-Acking peer over loopback; if that is impractical
  (as with the #90 field case), a component-level test against the real
  `publish_self_card_update` / `send_card_to_contact` is acceptable, matching
  the 2.C/3.C precedent.

## Out of scope

- The #107 fix (welcome-sweep rebuilds the Welcome per-connection / the
  `Psk(KeyNotFound)` ADR-0009 rebind) — separate spec.
- Any change to the two-PSK genesis, group creation timing, or the MLS state
  machine.
- Removing/altering the #101 pending UI or the #109 removal path.

## Acceptance criteria

- After `add_contact` to an unreachable/never-joining peer, the invitee emits
  **no** MLS application frames (text, ContactCard, or attachment) for the
  unjoined group — zero `WrongGroupId` at a responder.
- A ContactCard broadcast (`publish_self_card_update`) skips pending contacts
  and still cards connected ones.
- The post-Ack self-card still sends (gate does not over-block).
- `send_message` behavior is unchanged (still "not connected yet").
- Local gate green: `cargo fmt`, `clippy -D warnings`, `cargo test` (incl. the
  new guardrail), plus the existing skattr-tests suite.
