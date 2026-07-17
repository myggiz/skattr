# ADR 0011 — Encode the `Kind::File` manifest as a CBOR byte string (`serde_bytes`)

**Status:** Accepted
**Date:** 2026-07-17
**Context:** Field testing (v0.1.1) found attachments larger than ~15.6 MiB
silently never send. Root cause is the CBOR encoding of the attachment manifest
carried in the MLS envelope. Issue #75.
**Relates:** Phase 3.A (attachment core) — the `AttachmentManifest` rides in an
MLS message via `Kind::File { manifest: Vec<u8> }`
(`crates/core/src/envelope/kinds.rs`). ADR 0010 (direct chunk frames) is
untouched — this changes only how the *manifest* serializes, not any chunk
frame. ADR 0006 (frozen mailbox protocol) is untouched.
**Requires a second reviewer** (wire-format change, per CLAUDE.md).

---

## Context

An attachment's `AttachmentManifest` (chunk hashes + `file_key` + metadata) is
CBOR-encoded and carried inside an MLS application message as
`Kind::File { manifest: Vec<u8> }`. The whole MLS envelope is then sent as a
single `Frame::MlsApp`, and `connection::send` **hard-rejects any inner frame
over 65 519 bytes** (Noise 65 535 − 16-byte ChaChaPoly tag); there is no
manifest fragmentation.

`Vec<u8>` with serde's default derive serializes as a **CBOR array of
integers**: every byte ≥ 24 costs 2 CBOR bytes (`0x18` + value), so a manifest
of *N* bytes encodes to ~1.9 *N*. This nearly doubles the manifest on the wire:

| File | Chunks | Raw manifest | On the wire (array) | vs 65 519 cap |
|---|---|---|---|---|
| 6.3 MiB | 135 | 13.4 KB | 26.7 KB | fits |
| 20.4 MiB | 436 | 43.1 KB | **85.5 KB** | **exceeds** |

The practical failure threshold was ~15.6 MiB. Above it the `MlsApp` frame is
rejected by `send`; the per-peer actor discarded the error via `.is_err()` and
treated it as a link failure, so nothing surfaced — the attachment (not even the
filename) never appeared on the receiver. `serde_bytes` is already used
correctly on the direct `Chunk` frame's ciphertext (`transport/frame.rs`); it
was simply missing on the manifest field.

## Decision

Annotate the manifest field with `#[serde(with = "serde_bytes")]`:

```rust
File {
    #[serde(with = "serde_bytes")]
    #[ts(type = "string")]
    manifest: Vec<u8>,
},
```

This serializes the manifest as a **CBOR byte string** (~1 byte/byte + a small
length header) instead of an integer array. A 20.4 MiB file's manifest drops
from ~85.5 KB to ~43 KB — comfortably under the 65 519-byte cap.

Two regression tests lock the behavior
(`crates/core/src/envelope/kinds.rs`): `…encodes_as_compact_byte_string`
(a 1000-byte manifest must encode to < 1100 bytes — impossible for the array
form) and `…decodes_legacy_integer_array_encoding` (see backward-read below).

## Consequences

- **This is a wire-format change** (CBOR array → CBOR byte string) to the
  MLS-carried `Kind::File` payload. Consistent with ADR 0010's stance, v0.1.x
  has **no version negotiation** — both peers are assumed to run compatible
  builds. A `SendFile` between mismatched builds is not a supported
  configuration.
- **Backward-read compatible (verified).** `serde_bytes` on *deserialize*
  accepts both a byte string and a u8 sequence, so a peer on the new encoding
  can still decode a manifest that was sent under the old array encoding
  (proven by `…decodes_legacy_integer_array_encoding`, which encodes the legacy
  array shape via a mirror enum and decodes it as the real `Kind`). The reverse
  — an *un*-upgraded peer reading a new byte-string manifest — is not relied
  upon and not supported.
- **JSON / UI unaffected.** The change alters only CBOR (the MLS wire). Across
  the Tauri boundary the manifest is serialized by `serde_json`, whose
  `serialize_bytes` still emits a JSON number array; the ts-rs binding keeps its
  `#[ts(type = "string")]` annotation. The UI decoder, which passes the runtime
  number array straight to the Rust manifest decoder, needs no change (verified:
  core suite + attachment integration guardrails green, `pnpm check` green).
- **Raises the threshold, does not remove the ceiling.** The fix moves the
  practical limit from ~15.6 MiB to ~31 MiB. Beyond ~31 MiB the *raw* manifest
  itself exceeds the 65 519-byte frame cap, so a manifest-fragmentation /
  chunked-manifest path is still required for very large attachments — tracked
  separately (the deferred chunked-send path; issue #75 notes this, and the
  offline lane is bounded by `MAX_OFFLINE_ATTACHMENT_BYTES`).
- **Send failures are now logged.** The per-peer actor's send arm binds the
  error instead of discarding it via `.is_err()` and emits a `warn!`, so an
  oversized-frame (or any send) failure is diagnosable in logs. A *user-visible*
  (in-UI) send failure depends on sender-side delivery status, which remains a
  v1.1 item.

## Alternatives considered

- **Fragment / paginate the manifest across multiple MLS messages.** The general
  fix for arbitrarily large manifests, but a larger protocol change (ordering,
  reassembly, a manifest-part frame). Deferred; `serde_bytes` is the small,
  correct fix that resolves the reported field failure (files up to ~31 MiB) and
  is a strict prerequisite either way.
- **base64-encode the manifest into a `String`.** Also ~1.33× on the wire and
  loses the byte-string efficiency; pointless versus `serde_bytes`. Rejected.
- **Raise the frame cap / add transport fragmentation for `MlsApp`.** Changes a
  load-bearing Noise invariant for every message type to work around a manifest
  encoding bug. Rejected — fix the encoding.
