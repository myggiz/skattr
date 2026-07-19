# ADR 0012 — First-contact idempotent re-Ack for a lost first-contact Ack

**Status:** Accepted
**Date:** 2026-07-19
**Context:** #93 first-contact-recovery redesign. A lost first-contact
`Frame::Ack` leaves the invitee stuck in `PendingJoin` because the re-sent
Welcome is rejected as an unknown `kp_ref`.
**Relates:** ADR 0007 (first-contact Welcome carve-out + transport↔MLS identity
binding); ADR 0009 (`h_transport` ↔ MLS binding); ADR 0006 (mailbox/wire
protocol, frozen). Tied to #90 (Mode A circuit teardown); this is part of the
#93 first-contact-recovery work (invitee `PendingJoin` re-send).
**Requires a second reviewer** (auth/protocol change, per CLAUDE.md). Written
before the code (Tasks 9–11) per CLAUDE.md's "protocol changes need an ADR
before code" rule.

---

## Context

First contact is asymmetric (ADR 0007). The **invitee** (committer) builds the
genesis MLS group from the inviter's KeyPackage, produces a **Welcome for the
inviter**, and dials the inviter to deliver it (`Frame::MlsWelcome`). The
**inviter** (responder) accepts the connection, runs the invite-gated Welcome
bootstrap, joins the group, and replies with `Frame::Ack(welcome_msg_id(...))`.
The invitee stays `PendingJoin` until it sees that Ack.

The responder's join is **destructive to the invite**. In
`dispatch_welcome_inner` (`crates/core/src/daemon/inbound.rs`) the whole
join-and-persist runs in one `pool.transaction`, whose final acts consume the
single-use invite:

```
kp_repo.mark_consumed_in_tx(tx, &kp_sha256)?;   // inbound.rs:583
oi.mark_consumed_in_tx(tx, &kp_ref)?;           // inbound.rs:584
```

After that transaction commits, the responder is a full member and sends its
`Frame::Ack`. **If that Ack is lost** — the concrete trigger is #90 Mode A,
where the onion circuit that carried the Welcome is torn down before the Ack
frame is flushed back — the invitee never leaves `PendingJoin` and, per #93,
re-sends the **byte-identical** Welcome on a fresh connection.

The re-sent Welcome now fails. The responder's Welcome path re-derives the
`kp_ref` and looks it up in `outstanding_invites`
(`get_psk_and_kp_bytes(&kp_ref)`), but that invite was consumed on the first
pass, so the lookup returns `None`:

```
CoreError::from(MlsErrorKind::Other("inbound welcome: unknown kp_ref".into()))
// inbound.rs:471–473
```

The dispatch returns `Err`, so the accept-loop / actor arm logs
`"inbound: dispatch_welcome failed, not ACKing"` and returns `None`
(`inbound.rs:670`) — **no Ack is sent**. First contact is permanently stuck.

Re-running the MLS join for the re-sent Welcome is **not** a viable fix.
`h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` (ADR 0009) binds
the genesis Commit to **one** Noise session. The re-sent Welcome arrives over a
*new* connection with a *different* handshake hash, hence a different
`h_transport`; the PSK the genesis Commit references cannot be reproduced, so
re-processing the Welcome would fail the binding even if the invite were still
live. The responder has already joined — it should not, and need not, join
again. What it must do is **re-emit the Ack it already earned**.

## Decision

Add a **durable, Noise-authenticated, MLS-free idempotent re-Ack** for a
re-presented first-contact Welcome.

### 1. Record the first-contact join durably

When the responder successfully bootstraps a first-contact Welcome (inside the
same `pool.transaction` that consumes the invite and persists the group), it
also records the join in a new table:

```text
first_contact_acks(
    kp_ref        BLOB PRIMARY KEY,   -- the invite's canonical KeyPackageRef (32 bytes)
    peer_x25519   BLOB NOT NULL,      -- the authenticated Noise static of the joining peer
    peer_identity BLOB NOT NULL,      -- the derived Ed25519 identity (the resolved contact)
    created_at    INTEGER NOT NULL
)
```

The write is part of the join transaction, so the record exists **iff** the
join committed — it cannot describe a join that did not happen, and it survives
process restart (the Ack must be recoverable across the crash window #90 opens).

### 2. Idempotent re-Ack on a matching re-presented Welcome

On any first-contact Welcome, **before** attempting the (now-impossible) MLS
join, the responder checks `first_contact_acks` for the Welcome's `kp_ref`:

- **`kp_ref` matches a record AND the authenticated `peer_x25519` equals the
  recorded `peer_x25519`** → this is the same peer re-presenting the same
  Welcome after a lost Ack. Return the **stored `peer_identity`** as an
  idempotent re-Ack: re-send `Frame::Ack(welcome_msg_id(welcome))` over the
  connection and **do not re-run the MLS join** — no `outstanding_invites`
  lookup, no PSK / `h_transport` derivation, no `join_from_welcome`, no
  group-state mutation, no new transaction. The responder is already a member;
  this path only re-transmits the acknowledgement.

- **`kp_ref` matches a record but the authenticated `peer_x25519` differs** →
  **reject** (do not Ack, do not join). A `kp_ref` is bound to exactly the one
  peer that first consumed it; a different Noise static presenting the same
  `kp_ref` is a replay by another party, and honouring it would violate
  KeyPackage single-use. This preserves the ADR 0007 single-use guarantee across
  the re-Ack path.

- **No record for `kp_ref`** → fall through to the existing first-contact join
  path (ADR 0007 + ADR 0009), unchanged.

### 3. No wire-format change

The re-Ack reuses the existing `Frame::MlsWelcome` (request) and `Frame::Ack`
(response). No new `Frame`, `Command`, `CommandResult`, or `Event` variant is
introduced. **ADR 0006 stays frozen.**

## Security analysis

- **The re-Ack never touches MLS state.** It is a pure lookup-and-retransmit: no
  Commit is processed, no epoch advances, no leaf/PSK is registered, no group
  bytes are written. A malicious, stale, replayed, or corrupted duplicate
  Welcome therefore **cannot** mutate or corrupt the existing group through this
  path — the worst it can achieve (when it also matches the recorded
  `peer_x25519`, i.e. it *is* the authentic peer) is provoke a redundant Ack for
  a group it is already a member of.

- **Re-Ack cannot be provoked for another identity.** The peer is
  Noise-authenticated: `peer_x25519` is the verified static key of the live
  connection. The re-Ack fires only when that authenticated key equals the
  `peer_x25519` recorded at join time, and it returns the identity recorded then
  — an attacker on a different connection (different Noise static) is rejected by
  the `peer_x25519` mismatch arm, so it cannot cause the responder to
  acknowledge or re-attribute the contact to a party of the attacker's choosing.

- **No secret material on the re-Ack path.** Unlike the join path (ADR 0009),
  the re-Ack derives no `h_transport`, registers no PSK, and reads no invite
  secret. `welcome_msg_id(welcome)` is a public message id. The only stored
  material is the public `peer_x25519` and public Ed25519 `peer_identity`, plus
  the public `kp_ref`.

- **KeyPackage single-use is preserved.** The invite is consumed exactly once
  (in the original join transaction). The re-Ack does not re-consume or
  re-validate the invite; it is gated by the `first_contact_acks` record and the
  `peer_x25519` match, which is *stricter* than the original invite gate (it
  additionally pins the peer). A second, distinct peer presenting the same
  `kp_ref` is rejected outright.

## Relationship to ADR 0007 and ADR 0009

- **ADR 0007 (Welcome carve-out).** The re-Ack is a **refinement** of the ADR
  0007 first-contact carve-out for the *re-presented* Welcome case. ADR 0007
  gives an unknown peer exactly one chance to present a valid Welcome and, on
  success, records the contact and Acks. This ADR handles the follow-up where
  that Ack was lost: the same peer re-presents the same Welcome, and the
  responder — which already ran the ADR 0007 bootstrap once — replies from the
  durable record instead of re-running a bootstrap that can no longer succeed.

- **ADR 0009 (`h_transport` binding).** The re-Ack **deliberately bypasses** the
  ADR 0009 transcript binding, because it does **not re-join**. `h_transport`
  binds the genesis Commit to the one Noise session that produced it; the
  re-sent Welcome arrives on a different session and could never reproduce that
  binding. Since no Commit is processed on the re-Ack path, there is nothing to
  bind — the ADR 0009 guarantee remains intact for the *original* join (the only
  place a group is ever formed), and the re-Ack adds no new binding obligation.

## Consequences

- **Lost first-contact Ack self-heals.** The invitee's re-sent Welcome (#93) is
  answered from the durable record, the invitee's Ack resolves, and it leaves
  `PendingJoin` — closing the #93 permanent-stall.

- **Lost first-contact *Welcome* is not covered.** If the responder **never
  joined** (the *first* Welcome was lost, not its Ack), there is no
  `first_contact_acks` record, so the re-sent Welcome falls through to the join
  path — which cannot recover over a since-replaced circuit, because the
  re-sent Welcome's `h_transport` will not match the genesis Commit's PSK (ADR
  0009). This is disclosed as a **v1.0 limitation** tied to #90 Mode A. It is
  **not a regression**: a truly-lost first Welcome already stalled first contact
  before #93 (the responder simply never received anything to act on). Recovering
  it requires re-issuing the invite, or the deferred first-contact-over-mailbox
  work.

- **New durable state.** A `first_contact_acks` table (new migration) plus a
  small repo to insert (inside the join transaction) and look up by `kp_ref`.
  New `pub(crate)` surface only — nothing is added to `core`'s public API.

- **Wire-format neutral.** No new frames or IPC types; ADR 0006 unchanged. The
  responder's Welcome-handling arm gains a pre-join branch that returns the
  stored identity (re-Ack) or falls through to the existing bootstrap.

- **Guardrail.** The #93 fix must be proven through the real assembly: a
  first-contact flow where the responder's first Ack is dropped, the invitee
  re-sends the identical Welcome, and the second attempt yields an Ack that moves
  the invitee out of `PendingJoin` — with an assertion that the responder's MLS
  epoch did **not** advance on the re-Ack.
