# ADR 0006 — Mailbox protocol v1 (frozen)

**Status:** accepted
**Date:** 2026-04-28
**Predecessor ADRs:** 0001 (license), 0002 (crypto), 0005 (Arti).
**Related spec:** `docs/superpowers/specs/2026-04-27-phase-2a-mailbox-server-design.md`.

## Context

Phase 2.A delivers `crates/mailbox/` as the standalone, AGPLv3
mailbox server, with shared wire types in `core::mailbox::protocol`.
2.B (the client) consumes those types unchanged. To prevent silent
breakage between sub-projects, this ADR freezes the v1 wire surface
and records the rule for future evolution.

## Decision

The wire types declared in `core::mailbox::protocol` as of the merge
PR for Phase 2.A are **frozen**. Every C→S request body carries
`version: u16 == PROTOCOL_VERSION = 1`. The frame-byte assignment
(0x82–0x8F) and the `ErrorCode` enum are also frozen.

Incompatible changes — adding a required field, removing a field,
changing a field's CBOR type, renaming an existing variant, repurposing
a frame byte, reordering fields in the auth-digest tuple — ship as
`MAILBOX_PROTOCOL_V2`, a parallel module under
`core::mailbox::protocol_v2`. Servers may advertise both; clients
choose at connect time. v1 stays supported for at least one full
release after v2 ships.

Additive evolutions — adding a new optional field with a `#[serde(default)]`
default, adding a new `ErrorCode` variant — are compatible and may be
made within v1. Adding a new frame byte requires v2 because v1 decoders
reject unknown bytes.

## Wire types (canonical reference)

See `crates/core/src/mailbox/protocol.rs` at the merge commit. Mirror
of the freeze table:

| Type byte | Variant            | Direction | Body                                                                                                             |
|-----------|--------------------|-----------|------------------------------------------------------------------------------------------------------------------|
| 0x82      | `Deposit`          | C→S       | `version: u16, recipient_hash: [u8;32], ciphertext: bytes, ttl_request: u32`                                     |
| 0x83      | `DepositOk`        | S→C       | `deposit_id: [u8;16], expires_at: i64`                                                                            |
| 0x84      | `Challenge`        | C→S       | `version: u16, identity_hash: [u8;32]`                                                                            |
| 0x85      | `ChallengeNonce`   | S→C       | `nonce: [u8;32], issued_at: i64`                                                                                  |
| 0x86      | `Fetch`            | C→S       | `version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64]`                                      |
| 0x87      | `FetchResponse`    | S→C       | `deposits: Vec<{deposit_id: [u8;16], ciphertext: bytes, received_at: i64}>`                                       |
| 0x88      | `Delete`           | C→S       | `version: u16, identity_pubkey: [u8;32], nonce: [u8;32], signature: [u8;64], deposit_ids: Vec<[u8;16]>`           |
| 0x89      | `DeleteOk`         | S→C       | `deleted: u32, not_found: u32`                                                                                    |
| 0x8F      | `Error`            | S→C       | `code: ErrorCode, message: String`                                                                                |

`ErrorCode`: `UnsupportedVersion`, `MalformedRequest`, `TooLarge`,
`RateLimited`, `RecipientFull`, `TtlTooLong`, `TtlTooShort`,
`InvalidSignature`, `HashMismatch`, `NonceExpired`, `NotFound`, `Internal`.

## Auth string

```
"skattr-mailbox-auth-v1" || nonce || op_byte || sha256(positional_cbor_tuple)
```

`op_byte ∈ {0x86 (Fetch), 0x88 (Delete)}`.

`positional_cbor_tuple` is encoded by `ciborium` as a CBOR
**definite-length array** (positional — no field-name keys). The
tuple shape is **frozen**:

- For Fetch: `(version: u16, identity_pubkey: [u8; 32], nonce: [u8; 32])`
- For Delete: `(version: u16, identity_pubkey: [u8; 32], nonce: [u8; 32], deposit_ids: Vec<[u8; 16]>)`

**Rationale for the positional tuple form** (from Phase 2.A's Task 16
tripwire): `ciborium`'s serde-derive encoding emits struct fields in
declaration order, not sorted. A "canonical CBOR map" implementation
using a struct would silently produce different digests if a future
contributor reordered fields for cosmetic reasons. The positional
tuple has no such ambiguity — order is the spec, encoded directly as
a CBOR array.

Reordering the tuple fields is incompatible and requires
`MAILBOX_PROTOCOL_V2`.

## Test bar (satisfied)

The merge PR satisfies all six layers from the spec:

- [x] Unit tests in every module touched.
- [x] Property tests round-trip every frame and every `ErrorCode`,
      plus tuple-digest determinism + position-binding properties.
- [x] Fuzz harness present (`crates/mailbox/fuzz/`); manual ≥ 1 hour
      run with no findings is the freeze-PR validation step.
- [x] Adversarial regression suite triggers every `ErrorCode` variant
      across four files (auth, policy, storage, codec).
- [x] 24 h soak driver landed (`#[ignore]`-gated); summary committed
      at `docs/superpowers/runs/<merge-date>-mailbox-soak.txt` is the
      freeze-PR validation step.
- [x] Real-Tor smoke test landed (`crates/tests/src/mailbox_real_tor.rs`,
      `#[ignore]`-gated); manual run is the freeze-PR validation step.
- [x] Logging-redaction unit test enforces no full hashes / pubkeys /
      ciphertexts at `info+`.

## Consequences

- 2.B's `MailboxClient` writes against this freeze and ships without
  surprise breakage from 2.A churn.
- Any post-2.A change that touches `core::mailbox::protocol` requires
  either a v2 ADR (incompatible) or a one-paragraph addendum to this
  ADR (compatible additive).
- The frozen frame bytes (0x82–0x8F) are off-limits for any other
  protocol added to skattr.
- The auth-digest tuple shape is part of the wire contract; reordering
  silently breaks signatures across versions, so the `dispatch::handle_*`
  tuple expressions are load-bearing and should be flagged in any
  future refactor.

---

## Clarifying note (2026-08-07, non-normative — no wire change)

The *Wire types* table above writes `ciphertext: bytes`. Read "bytes" as
**"opaque payload"**, not as CBOR major type 2 (byte string).

As implemented, `Deposit.ciphertext` and `PendingDeposit.ciphertext` are plain
`Vec<u8>` with **no `#[serde(with = "serde_bytes")]`** (`mailbox/protocol.rs`).
Under `ciborium` that encodes as a CBOR *array of integers* — roughly 1.9 bytes
on the wire per payload byte — rather than a compact byte string. This is the
same inflation ADR 0011 was written to fix for the attachment manifest.

It matters more than it used to: Phase 3.C deposits every offline attachment
chunk through this path, so offline attachment transfer pays the ~1.9× overhead.

**This is not fixable inside v1.** Adding `serde_bytes` changes the encoding and
is therefore an incompatible wire change — it belongs to a future v2 alongside
any other batched breaking changes. The note is recorded here so the discrepancy
between the table's wording and the shipped encoding is not mistaken for a bug,
and so v2 picks it up deliberately.
