# ADR 0007 — First-contact Welcome carve-out + transport↔MLS identity binding

**Status:** Accepted — **fully shipped.**
**Date:** 2026-06-12

> **History (resolved):** the carve-out + binding landed in Phase 1B (Task 9),
> and this ADR was originally accepted as a "down-payment" while the full
> first-contact `invite → add → first-message` flow it enables (Welcome-arm
> dial-on-demand, inviter-onion bootstrapping, card exchange) was still deferred
> to Phase 1C. **Phase 1C shipped** (merge `5c0b827`), so that deferral is
> closed — nothing in this ADR is outstanding.
**Context:** Phase 1B (direct P2P transport wiring). Surfaced by the Phase 1B
regression guardrail (`crates/tests/src/daemon_run_direct.rs`).
**Supersedes/relates:** Phase 2.E Welcome propagation; the inbound accept loop
added in Phase 1B (`crates/core/src/daemon/accept.rs`); the deferred
`h_transport` MLS binding (T1-1, Phase 2).
**Requires a second reviewer** (auth/protocol change, per CLAUDE.md). The
subagent spec-compliance and code-quality reviews satisfy this.

---

## Context

Phase 1B wired the inbound accept loop into `Daemon::run`. It authenticates each
inbound Noise connection and resolves the peer with
`ContactRepo::find_by_noise_x25519(outcome.peer_x25519)`, **rejecting any peer
that is not already a contact** (closing a latent auth gap — ordinary messages
from unknown peers must not reach the MLS pipeline).

The Phase 1B guardrail (two real daemons over an in-process transport, driving
`invite → add → bidirectional send`) revealed that this gate **deadlocks first
contact**:

1. Alice mints an invite (she does not yet know Bob). Alice stores the invite
   PSK + her KeyPackage in `outstanding_invites`, keyed by the canonical
   `KeyPackageRef` (`kp_hash`).
2. Bob runs `AddContact`: builds the MLS group from Alice's KeyPackage, produces
   a **Welcome for Alice**, persists Alice as a contact (identity + onion from
   the invite URL), and dials Alice to deliver the Welcome
   (`hub.send_welcome` → `Frame::MlsWelcome`).
3. Alice's accept loop completes the Noise handshake (learning Bob's X25519
   static, `outcome.peer_x25519`) and calls `find_by_noise_x25519`. **Bob is not
   a contact yet** — the Welcome is precisely what would make him one — so Alice
   **rejects the connection and drops the Welcome**.
4. Alice's group stays `PendingJoin` forever; the reverse direction never works.

Every prior test passed because it hand-wired `DeliveryHub::ingest` via
`test_exports`, bypassing the accept loop. The guardrail through the real
assembly is what exposed this.

### The identity problem

`dispatch_welcome_inner` (`daemon/inbound.rs`) validates a Welcome **independently
of the sender's transport identity** — it extracts the `KeyPackageRef` from the
Welcome, looks up the PSK in `outstanding_invites`, checks expiry, and
`Group::join_from_welcome`s. Good: the inviter can authenticate a Welcome with no
prior knowledge of the sender.

But it then **persists the new contact using a passed-in Ed25519 `peer`**
argument. The accept loop only has the sender's **X25519** noise static
(`outcome.peer_x25519`), and X25519 → Ed25519 is one-way. So the accept loop
cannot supply the correct Ed25519 identity. The contact's Ed25519 identity must
instead be **derived from the joined MLS group** (the other member's MLS
signature key, which equals their Ed25519 identity under our ciphersuite
`…_Ed25519`).

## Decision

Add a **narrow, invite-gated, identity-bound carve-out** to the inbound accept
loop, plus a self-attributing Welcome bootstrap and a transport↔MLS identity
binding check.

### 1. Accept-loop carve-out (`daemon/accept.rs`)

When `find_by_noise_x25519(outcome.peer_x25519)` returns `Ok(None)` (unknown
peer), do **not** immediately reject. Instead, read **exactly one** frame from
the authenticated connection, under a bounded timeout:

- **`Frame::MlsWelcome(bytes)`** → attempt the Welcome bootstrap (step 2). On
  success, send `Frame::Ack(welcome_msg_id(bytes))` over the connection (so the
  sender's `WelcomeJob` ACK resolves), then `hub.ingest(peer, conn)` under the
  **derived, binding-verified** peer so subsequent app frames flow.
- **Anything else, a read error, or a timeout** → reject + `conn.close()`
  (unchanged behavior). An unknown peer gets exactly one chance to present a
  valid Welcome; nothing else from an unknown peer reaches the MLS app pipeline.

Known peers (the `Ok(Some(peer))` arm) are unchanged: ingest immediately.

### 2. Self-attributing, identity-bound Welcome bootstrap

Add `InboundDispatch::dispatch_welcome_bootstrap(&self, welcome: &[u8],
expected_x25519: &[u8; 32]) -> Option<PublicKey>` (defaulted to `None`).
`DaemonInbound` implements it as a refactor of `dispatch_welcome_inner` that:

1. Validates the Welcome via `outstanding_invites` (KeyPackageRef → PSK, expiry,
   not-consumed) — **peer-independent**, exactly as today.
2. `Group::join_from_welcome`.
3. **Derives the invitee's Ed25519 identity** from the joined group via a new
   `Group::peer_identity() -> Result<PublicKey>` (the non-self member's MLS
   signature key; mirrors the existing `own_public_key` accessor that reads
   `own_leaf_node().signature_key()`).
4. **Binding check (security-critical):** require
   `ed25519_pub_to_x25519(derived_peer) == *expected_x25519`. In the honest
   flow the invitee dials with its own identity as the Noise static, so the
   handshake's `peer_x25519` equals `ed25519_pub_to_x25519(invitee_identity)`. A
   mismatch means the Welcome was delivered over a connection **not bound to the
   invitee's identity** (a relay/MITM) → **abort before committing** (return
   `None`; do not join/persist/consume the invite).
5. On match, atomically (one `pool.transaction`): persist the group, upsert the
   contact under `derived_peer`, link `group_id`, mark the KeyPackage + invite
   consumed, emit `Event::ContactUpdated(derived_peer)`. Return
   `Some(derived_peer)`.

The existing `dispatch_welcome(peer, welcome)` (used by the per-peer actor's
read arm for Welcomes over an **already-established** connection) is retained and
refactored to share the join/derive logic; it additionally asserts the derived
identity equals the bound `peer` (defense in depth).

### 3. `Group::peer_identity()`

Add `pub(crate) fn peer_identity(&self) -> Result<PublicKey>`: iterate
`self.inner.members()`, return the member whose signature key ≠ our own leaf's
signature key, as a `PublicKey`. Errors if the group is not exactly 2-member
(our invariant) or no distinct peer is found.

## Security analysis

- **No weakening for ordinary messages.** Unknown peers still cannot deliver
  `Frame::MlsApp`/`MlsCommit`. The only inbound action an unknown peer can take
  is presenting **one** Welcome that must match a live, unexpired, unconsumed
  outstanding invite (PSK + KeyPackageRef) **and** be delivered over a
  connection bound to the invitee's own identity. Without the invite PSK no
  Welcome validates; without identity binding it is refused.
- **Anti-relay / anti-MITM.** The binding check
  (`ed25519_pub_to_x25519(derived) == peer_x25519`) prevents a third party from
  forwarding an authentic Welcome over their own transport connection and being
  recorded as the contact. This is a lightweight identity binding, **distinct
  from and compatible with** the deferred `h_transport` PSK injection (T1-1) —
  it binds the MLS member identity to the Noise static, not the handshake
  transcript.
- **Single-use.** The invite (KeyPackage + PSK) is marked consumed inside the
  same transaction, so a captured Welcome cannot bootstrap a second contact.
- **DoS.** One extra bounded frame-read per unknown inbound connection; the same
  connection-flood surface already noted for the accept loop (tracked for the
  Phase-2 semaphore/`JoinSet` hardening, `accept.rs` TODO). The bootstrap path
  does real MLS work only after the cheap `outstanding_invites` lookup succeeds.

## Alternatives considered

- **Invite-PSK Noise handshake (Noise_XK[psk3]).** Authenticate the invitee at
  the handshake layer using the invite PSK. More faithful to the locked Noise
  design, but larger, and overlaps Phase 2's `h_transport` work. Deferred; the
  application-layer carve-out is smaller and sufficient for first contact.
- **Reject + retry after a separate card exchange.** Would require an
  out-of-band path to teach the inviter the invitee's identity before the
  Welcome — there is none in the direct-only flow. Rejected.
- **Trust the passed-in peer from the actor.** Impossible here: the accept loop
  has no Ed25519 for an unknown peer. Self-attribution from the group is the
  only correct source.

## Consequences

- **Wire-format neutral.** No new `Frame`, `Command`, `CommandResult`, or
  `Event` variants. Reuses `Frame::MlsWelcome`, `Frame::Ack`, and
  `Event::ContactUpdated`.
- New `pub(crate)` surface: `InboundDispatch::dispatch_welcome_bootstrap`,
  `Group::peer_identity`. Internal only; not added to `core`'s public API.
- The accept loop gains a bounded first-frame read for unknown peers (a read
  timeout constant).
- The Phase 1B guardrail's bidirectional first-contact assertions pass; the
  `#[ignore]` is removed. This becomes the live CI guardrail the roadmap
  mandates.
- The invitee's **onion** still reaches the inviter separately (a
  `ContactCardUpdate` after the group is active), unchanged by this ADR; this
  ADR only fixes the Welcome's first-contact delivery, which is the prerequisite
  for that card exchange to have a live connection.
