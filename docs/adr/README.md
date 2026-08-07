# Architecture Decision Records

Decisions that are expensive to reverse — protocol, crypto, wire formats,
licensing, storage. **CLAUDE.md requires an ADR for any protocol-level change**
(frame types, invite fields, handshake binding, MLS ciphersuite) *before* the
code, plus a second reviewer.

## Index

| # | Title | Status | Decision in one line |
|---|---|---|---|
| [0001](0001-license.md) | License choice | Accepted | GPL-3.0-or-later for `core`/`cli`/`tests`; AGPL-3.0-or-later for `mailbox` (§13 closes the hosted-service loophole). |
| [0002](0002-crypto-libraries.md) | Cryptographic library choices | Accepted | `ed25519-dalek`, `chacha20poly1305`, `argon2` (64 MiB/t=3/p=4), `hkdf`, `sha2`, `blake2`, `snow`; Noise is plain `Noise_XK_25519_ChaChaPoly_BLAKE2s` in production. |
| [0003](0003-storage-approach.md) | Storage approach | Accepted *(amended 2026-08-07)* | `rusqlite` (bundled) + WAL, app-level `age` encryption at rest. Amendment records the shipped sentinel / crash-recovery / close lifecycle and the **pinned scrypt work factor** (12 encrypt, 22 decrypt ceiling). |
| [0004](0004-passphrase-normalization.md) | Passphrase Unicode normalization | Accepted | Passphrase bytes used verbatim, no normalization. **Note:** the promised UI NFC pass never shipped — the lockout risk is unmitigated. |
| [0005](0005-arti-vs-system-tor.md) | Embed Arti vs. system `tor` | Accepted *(corrected 2026-08-07)* | Embed Arti; no system-`tor` fallback shipped. **The `.onion` is not seed-derived** — restoring from seed alone yields a new address. |
| [0006](0006-mailbox-protocol-v1.md) | Mailbox protocol v1 | Accepted — **frozen** | Wire protocol frozen at v1; compatible changes need an addendum here, incompatible ones need v2. |
| [0007](0007-first-contact-welcome-carveout.md) | First-contact Welcome carve-out + transport↔MLS identity binding | Accepted — shipped | The accept loop carves out an unauthenticated first-contact `Welcome`, authenticating and binding the derived identity before join. |
| [0008](0008-invite-embeds-contact-card.md) | Invite embeds the inviter's signed ContactCard | Accepted — shipped (1C) | The invite carries the inviter's signed `ContactCard`, so the consumer learns the onion from the link. |
| [0009](0009-h-transport-mls-binding.md) | `h_transport` ↔ MLS binding (dial-first two-PSK genesis) | Accepted — **shipped & mandatory** | `h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` injected as an external MLS PSK on the genesis commit. A required security control. |
| [0010](0010-attachment-transport-frames.md) | Additive transport frames for direct attachment transfer | Accepted | Pull-driven `ChunkRequest`/`Chunk`/`ChunkNack`/`AttachmentComplete` at `0x0B`–`0x0E`; chunks are Noise-encrypted, not MLS-wrapped. |
| [0011](0011-attachment-manifest-serde-bytes.md) | Encode `Kind::File` manifest as a CBOR byte string | Accepted | `#[serde(with = "serde_bytes")]` on the manifest — avoids ~1.9× CBOR integer-array inflation. |
| [0012](0012-first-contact-idempotent-reack.md) | First-contact idempotent re-Ack | Accepted | A re-sent Welcome is idempotently re-Acked so a lost Ack self-heals. |

## Known gaps (locked decisions with no ADR)

Flagged by the v1.0 readiness audit and still open — all protocol-level:

- **MLS ciphersuite** — `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`
  (codepoint `0x0003`). Highest-stakes gap; the design doc's prose still names
  an abandoned 256-bit AES suite.
- **Noise pattern** — only a table cell in ADR 0002; no XK-vs-IK/KK rationale,
  no record of identity-key-as-Noise-static or the handshake-hash reuse.
- **Envelope wire format** — `Envelope`/`Kind`, the inert placeholder kinds, and
  the ±1 h `ts` window with its mailbox-path exemption.

## Conventions

- Filename `NNNN-kebab-title.md`, numbered contiguously; header fields
  `**Status:**` / `**Date:**`.
- **Status** is one of `Proposed`, `Accepted`, `Superseded by ADR NNNN`. Keep the
  value itself short — put caveats and history in the body, not the field.
- **Update the status when the code ships.** ADRs 0008 and 0009 sat at
  `Proposed` long after shipping (0009 describes a *mandatory* security
  control), which is what this index exists to prevent.
- Amend rather than rewrite: append a dated `## Amendment` section so the
  original decision and its evolution both stay readable.
- **Add the row here when you add an ADR.**
