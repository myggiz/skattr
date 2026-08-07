# ADR 0009 — `h_transport` ↔ MLS binding via dial-first genesis two-PSK commit

**Status:** Accepted — **shipped and mandatory.** Implemented in Phase 2.A
(merge `bc71f32`) as the dial-first two-PSK genesis commit described below.
The binding is active on the sole production genesis path: the invitee derives
`h_transport` from the live Noise session and injects it alongside the invite
PSK (`daemon/dispatch.rs`, `Group::create_solo` + `add_member`), and the
inviter registers the identical transcript value before `join_from_welcome`
(`daemon/accept.rs` → `daemon/inbound.rs`). A joiner that cannot resolve the
binding PSK fails the join; negative tests in `mls/group.rs` and
`dispatch_welcome_bootstrap_rejects_binding_mismatch` lock this in. Treat it as
a required security control, not an option.
**Date:** 2026-06-13 (accepted 2026-08-07 after verifying the shipped code)
**Context:** Phase 2.A (MLS ratchet & binding integrity). Implements the
audit's T1-1 and, in the same construction, fixes T2-8 (per-invite PSK
uniqueness).
**Relates:** ADR 0007 (transport↔MLS *identity* binding, shipped in 1B);
ADR 0008 (invite embeds the inviter's ContactCard); the locked design's
"`h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` injected as an
external PSK into the first MLS Commit."
**Requires a second reviewer** (crypto/protocol change, per CLAUDE.md). The
subagent spec-compliance + code-quality reviews satisfy this.

---

## Context

The locked design states the transport↔MLS binding `h_transport` is injected as
an external PSK into the first MLS Commit. In code it is **derived** per Noise
connection (`transport/noise.rs`, `HKDF(handshake_hash, "skattr-binding-v1")`)
and exposed (`AuthenticatedConnection::h_transport()`), but **never reaches the
MLS layer** — the dialer drops `_outcome`, the accept loop uses `outcome` only
for the x25519 lookup, and `mls/group.rs` registers only the *invite* PSK. The
binding is cryptographically absent.

Two facts shape the fix:

1. **Timing.** The genesis `add_member` Commit (the only Commit in 2-member
   v1.0 — there is no PCS) is built **synchronously in `add_contact`, before any
   Noise connection exists**. So `h_transport` cannot be injected into it as the
   code stands.
2. **An identity binding already ships.** ADR 0007 binds the MLS member identity
   to the Noise static key (`ed25519_pub_to_x25519(derived) == peer_x25519`).
   `h_transport` adds a distinct property: binding to the handshake **transcript**
   (defends against a relay that re-handshakes with the same identity keys but a
   different session).

A separate but adjacent bug (T2-8): the invite PSK is registered under a
**fixed** `PreSharedKeyId::external(b"skattr-binding-v1", [0u8;32])` — constant
id+nonce across all invites — so a second invite's PSK overwrites the first in
the provider store (masked only by single-group scope today). Any *second*
external PSK (such as `h_transport`) registered under a constant id would hit the
same overwrite. So the binding and the uniqueness fix must be designed together.

## Decision

**Dial-first genesis bind.** Reorder first contact so the invitee (the
committer) establishes the Noise connection *before* building the genesis
Commit, and inject `h_transport` as a **second external PSK** into that Commit
alongside the invite PSK. Both PSK ids are made **unique per invite** by
deriving them from the invite's `KeyPackageRef`.

### Invitee side (`daemon/dispatch.rs::add_contact`, the committer)
1. Resolve the inviter's onion from the invite-embedded ContactCard (ADR 0008)
   and **dial → `(conn, h_transport)`** (the dialer now returns `h_transport`
   instead of dropping it).
2. `hub.ingest(inviter, conn)` — the per-peer actor reuses this single
   connection for the Welcome and all subsequent messages (no second dial).
3. Build `Group::create_solo` + `add_member`, registering **two** external PSKs
   and proposing both in the genesis Commit:
   - the **invite PSK** under id `psk_id("invite", kp_ref)`,
   - **`h_transport`** under id `psk_id("htransport", kp_ref)`.
4. Persist {group, contact, card, group_id} **and** mark the KeyPackage consumed
   in **one `pool.transaction`** (also closes T2-1; the dial happens before the
   transaction). Then send the Welcome over the ingested connection.

### Inviter side (`daemon/accept.rs` → `dispatch_welcome_bootstrap` → `welcome_join_persist`)
The inbound handshake yields the **same** `h_transport` (one Noise session ⇒
identical transcript hash on both ends). Thread it from the accept loop's
`outcome` into the Welcome-processing path; register **both** PSKs (invite +
`h_transport`, same derived ids) in the provider **before**
`join_from_welcome`, so OpenMLS validates the binding when it processes the
genesis Commit carried by the Welcome.

### PSK id derivation (closes T2-8)
```text
psk_id(label, kp_ref) = PreSharedKeyId::external(
    b"skattr-" ++ label ++ "-v1" ++ kp_ref,   // 32-byte KeyPackageRef ⇒ unique per invite
    nonce = kp_ref,                            // distinct per invite
)
```
Distinct `label`s ("invite" vs "htransport") keep the two PSKs in one commit
from colliding; the `kp_ref` suffix makes every invite's PSKs unique across
groups — no cross-invite/cross-group overwrite for *either* PSK.

## Security analysis

- **Transcript binding achieved.** The genesis Commit (the group's root of
  trust) cannot be processed by the inviter unless the inviter holds the same
  `h_transport` — i.e. participated in the *same* Noise session whose transcript
  produced it. A relay that re-handshakes with stolen identity keys gets a
  different `h_transport` ⇒ the Welcome fails to process. This composes with (does
  not replace) ADR 0007's identity binding and is independent of the deferred
  metadata-minimization work.
- **No unbound window.** The binding is on the genesis Commit itself; the group
  is transport-bound from epoch 1. (This is the advantage over a post-join PCS
  bind.)
- **PSK uniqueness.** Both external PSKs are keyed by the single-use
  `KeyPackageRef`, so registering them never overwrites a prior invite's PSKs
  (fixes T2-8). The invite remains single-use (KeyPackage consumed in the same
  transaction as the group write — T2-1).
- **No secret in logs / wire.** `h_transport` is a `Zeroizing<[u8;32]>`; it is
  registered into the OpenMLS provider and never logged or placed on the Skattr
  wire. The MLS Commit's PSK *proposals* reference the ids (not the secrets),
  exactly as the invite PSK already does.

## Alternatives considered

- **Post-join PCS bind** — leave the invite flow intact and bind via a later
  self-update Commit once both sides are connected. Rejected: leaves an unbound
  window after join, adds epoch churn + a post-join trigger, and depends on PCS
  machinery not otherwise needed in v1.0.
- **Retire the claim** — rely solely on ADR 0007's identity binding and strike
  the transcript binding from the design. Rejected by decision: the transcript
  binding is the property the threat model advertises and is worth the
  first-contact reorder.

## Consequences

- **`add_contact` is reordered** (dial-before-commit) and now couples to the
  dialer + `hub.ingest`. This reshapes the freshly-merged 1C add-contact path; the
  1C first-contact guardrail must continue to pass and is extended to assert the
  binding PSK is present in the genesis group.
- `h_transport` is plumbed: the dialer returns it; the accept loop forwards it to
  the Welcome-bootstrap. `create_solo`/`add_member`/`join_from_welcome` /
  `welcome_join_persist` / `dispatch_welcome_bootstrap` gain an `h_transport`
  parameter.
- **Wire-format neutral at the Skattr layer:** no new `Frame`/`Command`/
  `CommandResult`/`Event` variants. The MLS Commit bytes change (a second PSK
  proposal), but that is internal to MLS and there are no deployed groups
  (pre-v1).
- The constant `PSK_ID_BYTES = b"skattr-binding-v1"` is replaced by the
  `psk_id(label, kp_ref)` derivation; the `skattr-binding-v1` HKDF label on
  `h_transport` itself (in `identity/derive.rs`) is unchanged.
