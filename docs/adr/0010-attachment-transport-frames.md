# ADR 0010 — Additive transport frame types for direct attachment chunk transfer

**Status:** Proposed
**Date:** 2026-06-17
**Context:** Phase 3.B (direct attachment transfer). Adds the wire movement of
attachment chunks over the existing Noise channel, pull/request-driven, between
two online peers.
**Relates:** Phase 3.A (attachment core, merge `9a132d1`) — manifest in MLS via
`Kind::File`, opaque AEAD chunk blobs keyed from the manifest, `ChunkStore` +
`AttachmentRepo`. Design spec:
`docs/superpowers/specs/2026-06-17-phase-3b-direct-attachment-transfer-design.md`.
ADR 0006 (frozen mailbox wire protocol) is **not** touched — chunks move on the
direct transport, not the mailbox.
**Requires a second reviewer** (transport/protocol change, per CLAUDE.md). The
subagent spec-compliance + code-quality reviews satisfy this.

---

## Context

3.A produces, for a file, an `AttachmentManifest` (carried in an MLS message via
`Kind::File`) and a set of opaque per-chunk ciphertext blobs, each AEAD-sealed
with a per-chunk key derived from the manifest's `file_key`. 3.A stages and
reassembles these blobs **locally** — there is no path to move them between
peers.

The direct transport (`transport/frame.rs`) is a length-prefixed, type-tagged
frame codec running **inside** the Noise_XK channel established by the
handshake. Existing `FrameType`s occupy `0x01`–`0x0A`
(`NoiseInit`, `NoiseResp`, `MlsWelcome`, `MlsCommit`, `MlsApp`, `Ack`, `Ping`,
`Pong`, `Bye`, `Error`). The decoder rejects unknown types. The per-peer
`delivery::peer` actor owns the single live connection and multiplexes all frame
traffic for that peer through one `tokio::select!` loop.

We need to carry chunk requests and chunk data over this channel without:
- touching the frozen mailbox protocol (ADR 0006),
- double-encrypting (the chunk blobs are already AEAD ciphertext),
- adding a second connection or task per peer,
- starving interactive chat behind bulk transfer.

## Decision

Add **four additive `FrameType`s starting at `0x0B`** (the first free byte;
`0x0A` is `Error`), with CBOR payloads, carried on the same Noise channel as all
other frames:

| Byte | Frame | Direction | Payload |
|---|---|---|---|
| `0x0B` | `ChunkRequest` | receiver → sender | `{ attachment_id: [u8;16], index: u32 }` |
| `0x0C` | `Chunk` | sender → receiver | `{ attachment_id: [u8;16], index: u32, ciphertext: Vec<u8> }` |
| `0x0D` | `ChunkNack` | sender → receiver | `{ attachment_id: [u8;16], index: u32, reason: u8 }` |
| `0x0E` | `AttachmentComplete` | receiver → sender | `{ attachment_id: [u8;16] }` |

Properties:

1. **Pull / request-driven.** The receiver drives the transfer by requesting the
   indices it is missing; the sender serves `Chunk` reactively from its
   `ChunkStore`. The receiver-side resume state from 3.A
   (`attachment_chunks` / `received_indices`) is the source of truth for "what
   is still missing", so in-session resume needs no new persistence.

2. **No MLS wrapping of chunks.** A `Chunk`'s `ciphertext` is the 3.A per-chunk
   AEAD blob verbatim. It is **not** re-sealed through MLS. The chunk's integrity
   is verified against `manifest.chunks[index].ciphertext_hash` (SHA-256) after
   transport decryption and before storage; confidentiality comes from the
   Noise channel in transit and the manifest-derived AEAD at rest. The manifest
   itself — which carries `file_key` — travels in MLS (`Kind::File`), so the
   key material is never exposed to the transport layer.

3. **`ChunkNack`** signals the sender cannot serve an index (unknown attachment,
   index out of range, store read error — a small `reason` enum), letting the
   receiver fail fast rather than time out.

4. **`AttachmentComplete`** is the receiver's signal that all chunks are received
   and reassembled, so a pure-pull sender can mark its `out` row `complete`, GC
   its staged blobs, and emit send-side completion. Without it the sender has no
   way to learn the transfer finished.

5. **Flow control / fairness** is a property of the engine, not the wire: the
   receiver keeps ≤ 8 `ChunkRequest`s outstanding and only refills on idle
   capacity in the actor loop, so chat frames are never blocked behind chunk
   traffic. One active attachment per peer at a time (FIFO queue).

Frames are handled in the existing `conn.recv()` arm of `peer::full_run`; no new
connection or task is introduced.

## Consequences

- **Additive and backward-compatible at the type level**, but **not negotiated**:
  a peer that has not shipped 3.B will reject `0x0B`–`0x0E` as unknown frame
  types (the decoder errors). There is no capability handshake in v1.0; both
  peers are assumed to run compatible builds. A future version-negotiation step
  is out of scope here and noted for v1.1 if mixed-version interop becomes a
  goal. Until then, `SendFile` against an older peer fails the transfer cleanly
  (the manifest arrives, chunk requests are rejected) rather than corrupting
  state.
- **The frozen mailbox protocol (ADR 0006) is untouched.** Offline chunk
  transfer (3.C) will make its own decision (reuse `Deposit` vs. extend
  ADR 0006) in that phase's spec.
- **Chunk size on the wire** is bounded by the codec's existing 16 MiB frame cap;
  3.A's 256 KiB chunk size sits far under it, leaving headroom for the CBOR
  envelope.
- The decoder's unknown-type rejection now starts at `0x0F`.

## Alternatives considered

- **Push + per-chunk `Ack` (reuse `0x06`).** Simpler happy path, but the sender
  must persist per-chunk ack state to resume after a reconnect — new sender-side
  schema that 3.A does not have — and flow-control/interleaving becomes the
  sender's burden. Rejected in favour of pull, which reuses 3.A receiver state.
- **MLS-wrapping each chunk.** Redundant second AEAD over already-sealed blobs,
  and it would push 100 MiB of bulk data through the MLS ratchet (epoch/generation
  churn, ordering constraints). Rejected.
- **A dedicated second connection/stream for bulk transfer.** Avoids any
  head-of-line concerns but doubles connection/onion overhead per peer and
  complicates the actor model. Rejected; in-loop fairness (windowing) is
  sufficient for a 1:1 messenger.
