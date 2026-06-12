# ADR 0008 — Invite link embeds the inviter's signed ContactCard

**Status:** Proposed
**Date:** 2026-06-12
**Context:** Phase 1C (first contact over direct transport).
**Relates:** ADR 0007 (Welcome carve-out + transport↔MLS binding); Phase 1B
direct-transport assembly. Wire-format change to the `skattr://invite/v1#…`
link (the only one in 1C).
**Requires a second reviewer** (protocol/wire-format change, per CLAUDE.md). The
subagent spec-compliance + code-quality reviews satisfy this.

---

## Context

The Phase 1B direct-transport dialer (`delivery::dial::TransportDial`) resolves a
peer's onion **only** from a signed `ContactCard` via
`ContactRepo::latest_card(peer)?.body.onion`. There is no other onion source —
the `contacts` table has no onion column, and the legacy `onion_addresses` table
is unused by the dialer.

The current `InviteLinkBody` carries the inviter's `identity: PublicKey` and
`onion: String` (both covered by the inviter's Ed25519 signature over the link),
but **not** a `ContactCard`. So when the invitee runs `AddContact`, it has the
inviter's onion in hand yet no way to store it where the dialer reads it: a
`ContactCard` is signed over `ContactCardBody`, a *different* structure, and the
invitee cannot produce the inviter's card signature. Result (the 1B guardrail's
finding): the invitee cannot dial the inviter to deliver the Welcome, and even
if it could, has no persisted onion to resolve.

## Decision

**Embed the inviter's signed `ContactCard` in the invite link, replacing the
bare `identity` + `onion` fields** (the card supersets both).

```rust
// invite/link.rs
pub struct InviteLinkBody {
    pub card: ContactCard,   // inviter's signed self-card (identity + onion + mailboxes + version + expires_at)
    pub key_package: Vec<u8>,
    pub psk: [u8; 32],
    pub expires_at: i64,     // invite expiry (distinct from card.body.expires_at)
}
```

- `InviteLink::generate` builds the inviter's self-card via
  `contact::self_card::build_next_self_card` (monotonic version, current onion +
  reachable mailboxes, Ed25519-signed by the inviter) and embeds it.
- The **whole `InviteLinkBody` remains Ed25519-signed by the inviter** (the
  existing `InviteLink.signature`), binding the card + KeyPackage + PSK together
  as one unit so an attacker cannot swap the KeyPackage against a valid card.
- The embedded `ContactCard` additionally carries its **own** independent
  signature (the card standard), so the invitee can store it verbatim and the
  card stands alone in the card store.
- `AddContact` calls `ContactRepo::put_card(&link.body.card)` after the contact
  row exists, so `latest_card(inviter)` → the dialer resolves the inviter's onion
  with **no dialer or schema change**.
- Call sites that read `link.body.identity` / `link.body.onion` switch to
  `link.body.card.body.identity` / `link.body.card.body.onion`.

### Two signatures — why both

| Signature | Covers | Purpose |
|---|---|---|
| `InviteLink.signature` | the whole body (card + KP + PSK + expiry) | binds the card to *this* invite's KeyPackage + PSK; prevents KP/PSK substitution |
| `ContactCard.signature` | `ContactCardBody` only | lets the card be stored + later re-verified as a standalone card (rotation, `put_card` version guard) |

Both are the inviter's. The redundancy is intentional and cheap (64 bytes); each
authenticates a different unit.

## Security analysis

- **No weakening.** Verification on `from_url` still checks the inviter's link
  signature over the full body (`IdentityKey::verify_cbor`). The embedded card's
  own signature is additionally verifiable (and is verified again on any later
  `ContactCardUpdate`). The invitee stores an onion that is *doubly* the
  inviter's-signed — strictly stronger than today's bare signed onion.
- **Card/identity consistency.** `ContactCard::verify` returns the card's claimed
  identity; `AddContact` cross-checks `link.body.card.body.identity` is the
  identity it persists the contact under, so a malformed invite (card identity ≠
  intended) is rejected.
- **Onion freshness.** The embedded card is current at invite-creation time. If
  the inviter rotates later, the invitee learns the new onion via the normal
  `ContactCardUpdate` rotation path — unchanged.

## Consequences

- **Wire-format change to `skattr://invite/v1#…`** (the link body). Skattr is
  pre-v1 with no deployed invites, so there is **no back-compat burden**; the
  `v1` fragment tag is retained (no parallel format needed). The canonical-CBOR
  encoding + fragment-only param rule (design §invite) are unchanged.
- `InviteLinkBody` shrinks by two fields and gains one (`card`); the invite is a
  few dozen bytes larger (the card body + its signature), still well within a QR
  code.
- No `Command` / `CommandResult` / `Event` / `Frame` variant changes. The
  `key_package`, `psk`, `expires_at` fields and the single-use KeyPackage tracking
  are unchanged.
- `create_invite` now reads the daemon's onion (already required) and builds a
  self-card — it must run after Tor is ready (already the case; invite creation
  needs the onion today).
