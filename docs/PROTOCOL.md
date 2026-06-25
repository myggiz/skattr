# Skattr wire protocol

> **Status:** draft. This file will grow alongside the implementation.
> The authoritative source for protocol semantics today is
> [`skattr-design.md`](skattr-design.md); this file will progressively
> absorb it as implementations stabilize.

## Scope

This document defines the on-the-wire behaviour of Skattr clients and
the mailbox server. Two implementations that follow this spec should
interoperate even if one is older than the other, subject to the
version-negotiation rules in each layer.

## Layers

From bottom to top:

1. **Tor v3 onion services** — anonymizing transport. Out of scope for
   this document; see Tor Project specifications.
2. **Frame layer** — length-prefixed typed frames over a single Tor
   stream. See [`skattr-design.md` §1.2](skattr-design.md#12-transport-framing).
3. **Noise_XK transport handshake** — mutual identity-key
   authentication and forward secrecy. Pattern:
   `Noise_XK_25519_ChaChaPoly_BLAKE2s`. See
   [`skattr-design.md` §1.3](skattr-design.md#13-transport-handshake-noise_xk).
4. **Transport↔MLS binding** — `h_transport = HKDF(handshake_hash, "skattr-binding-v1")`
   injected as external PSK into the first MLS Commit.
5. **MLS** — group key agreement and application encryption.
   Ciphersuite `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`
   (IANA 0x0003). See
   [`skattr-design.md` §1.5](skattr-design.md#15-application-messaging-mls).
6. **Envelope** — CBOR application payload with `v`, `id`, `ts`, optional
   `reply_to`, `kind`. See
   [`skattr-design.md` §1.6](skattr-design.md#16-message-envelope-inside-mls-application_data).

## Invite links

URI scheme: `skattr://invite/v1#<params>`. Parameters in the fragment
to keep them out of HTTP referrer logs. See
[`skattr-design.md` §1.4](skattr-design.md#14-contact-exchange-invite-links).

## Mailbox protocol

A mailbox is itself a Tor onion service. Four operations:

- `DEPOSIT` — anyone can call; no auth.
- `CHALLENGE` — issues a 32-byte nonce for the authenticated operations.
- `FETCH` — recipient-authenticated pickup.
- `DELETE` — recipient-authenticated delete after successful pickup.

CBOR wire types live in `skattr_core::mailbox::protocol`. See
[`skattr-design.md` §3.2](skattr-design.md#32-mailbox-protocol).

## Versioning

Every versionable artefact embeds an explicit integer version:

- `skattr://invite/v1` — URI path segment.
- `Envelope.v` — single `u8` field.
- `ContactCard.version` — monotonic, signed; peers reject anything
  that isn't strictly greater than the last verified version.
- Mailbox protocol: `PROTOCOL_VERSION` constant in the shared module.

When bumping any of these, open an ADR describing the transition
strategy.
