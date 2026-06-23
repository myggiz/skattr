# Phase 3.B — Direct attachment transfer (design)

**Date:** 2026-06-17
**Status:** Approved (brainstorm) — ready for implementation plan
**Depends on:** Phase 3.A (attachment core, merge `9a132d1`) — complete.
**Scope boundary:** online, both-peers-reachable direct transfer only.
Out of scope: 3.C (offline mailbox-blob path + cross-session resume), 3.D
(Tauri attach/preview/progress UI).
**Companion ADR:** `docs/adr/0010-attachment-transport-frames.md` (additive
transport frame types — protocol change, requires a second reviewer per
CLAUDE.md).

---

## 1. Goal

Two online daemons round-trip a multi-chunk file end-to-end through the real
`run_with_transport` assembly over loopback: byte-identical on the receiving
side, metadata stripped, driven by a production `Command::SendFile`. 3.A built
the local, transport-free pipeline (chunk → encrypt → stage → reassemble); 3.B
adds the wire movement of chunks over the existing Noise channel and the
send/receive orchestration inside the per-peer `delivery::peer` actor.

## 2. Locked decisions (from the brainstorm)

1. **Pull / request-driven** chunk delivery. The receiver, holding the manifest,
   requests the indices it is missing; the sender serves them reactively.
   Chosen because the receiver-side resume state already exists in 3.A
   (`attachment_chunks` + `received_indices`/`mark_received`) and backpressure
   belongs on the consuming side. The sender keeps **no** per-chunk ack state.
2. **Auto-fetch** on manifest arrival (no UI in 3.B). The offer metadata is
   carried on the completion event rather than gated behind an explicit accept.
3. **Reassembled files land in a config-driven `download_dir`**, default
   `<data_dir>/downloads/`, with filename collision suffixing and filename
   sanitization (the manifest filename is attacker-controlled).
4. **Windowed flow control, N = 8** chunk requests in flight (≈0.4 MiB at the
   48 KiB chunk size — `CHUNK_SIZE` was reduced from 3.A's 256 KiB so one chunk
   fits one Noise message, ≤ 65 519 B); **one active attachment per peer at a
   time**, FIFO queue
   for the rest. Chat is never starved — chunk work only fills idle capacity in
   the actor's `select!` loop.
5. **In-session resume only.** A dropped/replaced connection within the session
   resumes from `received_indices`. Cross-session resume (daemon restart
   mid-transfer) is 3.C.
6. **Throttled progress events** — `AttachmentProgress` emitted at most every
   ~5 % or ~1 MiB, not per chunk.

## 3. Wire protocol — four additive frame types

Free `FrameType` bytes start at **`0x0B`** (`0x0A` is already `Error` — this
corrects the "0x0A+" note in PICKUP/CLAUDE.md). Chunk frames ride the **same
Noise channel** as `MlsApp`: transport-encrypted, **not** MLS-wrapped. The chunk
ciphertext is already AEAD-sealed by 3.A's per-chunk manifest keying, so no
second MLS layer is applied (or needed). Payloads are CBOR.

| Byte | Frame | Direction | Payload |
|---|---|---|---|
| `0x0B` | `ChunkRequest` | receiver → sender | `{ attachment_id: [u8;16], index: u32 }` |
| `0x0C` | `Chunk` | sender → receiver | `{ attachment_id: [u8;16], index: u32, ciphertext: Vec<u8> }` |
| `0x0D` | `ChunkNack` | sender → receiver | `{ attachment_id: [u8;16], index: u32, reason: u8 }` |
| `0x0E` | `AttachmentComplete` | receiver → sender | `{ attachment_id: [u8;16] }` |

- `AttachmentComplete` lets the sender mark its `out` row `complete`, GC its
  staged `ChunkStore` blobs, and emit send-side completion — a pure-pull sender
  otherwise cannot know the transfer finished.
- `ChunkNack` lets the receiver fail fast instead of hanging when the sender
  cannot serve an index. `reason` is a small enum (e.g. `0 = unknown attachment`,
  `1 = index out of range`, `2 = store read error`).

Touches: `transport/frame.rs` — `FrameType` enum + `Frame` enum + the
`FrameCodec` encode/decode match arms. The decoder's "unknown frame type" path
already rejects anything ≥ `0x0F`.

## 4. New unit — `delivery::chunk_transfer`

A per-peer transfer engine owned by the `peer` actor, with two halves sharing no
state beyond the actor's existing `pool` handle and a `ChunkStore`
(`<data_dir>/attachments`, already used by 3.A):

- **Receiver half** — holds the single active inbound `attachment_id` for this
  peer: the manifest, the in-flight request window, the per-index retry budget,
  and the received bitmap (read from `AttachmentRepo::received_indices`). Drives
  requests, verifies + stores arriving chunks, reassembles on completion.
- **Sender half** — stateless reactive server: answers a `ChunkRequest` by
  reading staged ciphertext from `ChunkStore`; emits `ChunkNack` on miss.

The engine is driven entirely from inside `peer::full_run`'s existing
`tokio::select!` loop — **no new task, no second connection.** Frames are read in
the same `conn.recv()` arm that already handles `MlsApp`/`Ack`/`Ping`.

## 5. Send path

New `Command::SendFile { contact: PublicKey, path: String }` (Command enum +
IPC/CLI wiring + a `CommandResult`). Handler (`daemon/dispatch.rs`):

1. **Read + cap** — read the file; reject if `> MAX_ATTACHMENT_BYTES` (100 MiB).
2. **Strip** — `strip::strip_metadata(bytes, mime)` → stripped bytes + effective
   mime (EXIF/ICC/text removed).
3. **Chunk** — `chunker::chunk_plaintext(stripped, filename, mime)` →
   `(AttachmentManifest, Vec<ciphertext>)` (fresh random `attachment_id` +
   `file_key`).
4. **Stage** — `ChunkStore::put(attachment_id, i, ct)` for every chunk.
5. **Persist** — `AttachmentRepo::insert(attachment_id, "out", manifest_cbor,
   total_chunks, now)`, status `pending`.
6. **Announce over MLS** — wrap `manifest.to_cbor()` in `Kind::File { manifest }`
   and send through the **existing** `SendMessage` path (inherits MLS sealing,
   outbox, retry, and direct→mailbox fallback for the manifest message). This is
   the only MLS message; chunks never touch MLS.
7. **Serve reactively** — the handler returns once the manifest is enqueued; it
   does **not** block on delivery. The sender's `peer` actor answers incoming
   `ChunkRequest` frames from `ChunkStore`. On `AttachmentComplete`: set the
   `out` row `complete`, remove staged chunks, emit send-side completion.

The sender keeps no per-chunk ack state — `ChunkStore` is the state; `received`
lives on the receiver.

## 6. Receive path

**Manifest arrival** (`daemon/inbound.rs`, where `Kind::File` is decrypted):

1. Parse + validate (`from_cbor`; reject if `total_size > MAX_ATTACHMENT_BYTES`
   or the chunk list is internally inconsistent).
2. **Sanitize filename** — strip path separators and `..`, drop control chars,
   cap length, preserve extension. The sanitized name is what lands on disk and
   rides the event.
3. `AttachmentRepo::insert(attachment_id, "in", manifest_cbor, total_chunks,
   now)`, status `pending`.
4. Signal the peer's actor with new `PeerCtrl::BeginInboundAttachment {
   attachment_id }` (enqueued FIFO; becomes active when no other inbound
   transfer runs for that peer).

**Fetch loop** (receiver half):

- `missing = manifest.chunks − received_indices(attachment_id)`.
- Keep **≤ 8** `ChunkRequest` frames outstanding. On each `Chunk`: verify
  `sha256(ciphertext) == manifest.chunks[index].ciphertext_hash` →
  `ChunkStore::put` → `AttachmentRepo::mark_received(index)` → refill the window
  with the next missing index → maybe emit throttled progress.
- **Interleave:** chunk frames are processed in the same `conn.recv` arm as
  `MlsApp`; the window only refills on idle capacity, so an inbound text message
  is never blocked behind chunk traffic.
- **Complete:** when `received == total`, reassemble via `StoreSource` +
  `reassemble(manifest, source, output_path)` to
  `<download_dir>/<sanitized-name>` (collision-suffixed); set status `complete`;
  send `AttachmentComplete`; emit `Event::AttachmentReceived` with offer
  metadata + final path; release the active slot and pull the next FIFO item.

**In-session resume:** a dropped connection is handled by the actor's existing
`PeerCtrl::ReplaceConn` reconnect path, which re-derives `missing` from
`received_indices` and resumes requesting. Already-stored chunks survive because
they are persisted to `ChunkStore` + `attachment_chunks` as they arrive. No new
persistence is introduced.

## 7. Error handling

Receiver-side unless noted:

- **Hash mismatch** on a `Chunk` → discard, re-request the same index. Bounded to
  **3 retries per index**, then fail the transfer.
- **`ChunkNack` received** → fail the transfer immediately.
- **Request timeout** — an outstanding `ChunkRequest` with no `Chunk`/`Nack`
  within **T = 30 s** → re-request (counts against the per-index retry budget).
  Connection-drop is handled by the reconnect/resume path, not this timer.
- **Fail** = set `AttachmentRepo` status `failed`, release the active slot,
  advance the FIFO queue, emit `Event::AttachmentFailed { attachment_id,
  reason }`. Reassembly is atomic (temp → rename), so nothing half-written
  reaches `download_dir`. Partial chunks remain in `ChunkStore` for later retry;
  GC of abandoned partials is a janitor concern noted as out of 3.B scope.
- **Oversize / invalid manifest** → never inserted, never fetched; logged and
  dropped.

## 8. Events & config

**New `Event` variants** (`daemon/events.rs`; all `Serialize` + `ts_rs::TS` so
3.D inherits them):

- `AttachmentReceived { contact, attachment_id, filename, mime, size, path }`
- `AttachmentProgress { attachment_id, received, total }`
- `AttachmentFailed { attachment_id, reason }`

Send-side completion is signalled by an `AttachmentProgress` reaching
`received == total` on the `out` side rather than a fourth variant. *(Open
choice for spec review: a dedicated `AttachmentSent` terminal event vs. reusing
terminal `AttachmentProgress`. Default: reuse.)*

**Config** (`Config` + `ConfigPatch`): add `download_dir: PathBuf`, default
`<data_dir>/downloads/`, created on first use. The window size (8), per-index
retry budget (3), and request timeout (30 s) remain **constants** in 3.B —
promotable to config in 3.D.

## 9. Assembly touchpoints

- `transport/frame.rs` — 4 frame types + codec arms.
- `delivery/chunk_transfer.rs` — new module (sender + receiver halves).
- `delivery/peer.rs` — `full_run` `select!`: handle the 4 new frames in the
  `conn.recv` arm; new `PeerCtrl::BeginInboundAttachment`; the receiver window
  refill logic; resume hook on `ReplaceConn`.
- `daemon/commands.rs` — `Command::SendFile` (+ `CommandResult`).
- `daemon/dispatch.rs` — `SendFile` handler (strip → chunk → stage → persist →
  announce).
- `daemon/inbound.rs` — `Kind::File` ingest → sanitize → persist →
  `BeginInboundAttachment`.
- `daemon/events.rs` — 3 events.
- `config` — `download_dir` field + patch.
- No migration: 3.A's `0015_attachments` schema already covers receiver state;
  the sender uses `ChunkStore` + the `out` row.

## 10. Deferred / explicitly-not-3.B

- **Offline manifest / online chunks gap:** if the `Kind::File` manifest is
  delivered via mailbox fallback while the sender is offline, the receiver has
  the manifest but cannot pull chunks until the sender returns. This is the
  offline transfer case — **3.C**. 3.B does not attempt it; the guardrail is
  both-peers-online.
- **Cross-session resume** (daemon restart mid-transfer) — 3.C.
- **Concurrent attachments per peer** (>1 active) — v1.1.
- **Abandoned-partial GC janitor** — noted, out of scope.
- **Configurable window/retry/timeout** — 3.D.

## 11. Testing / guardrail

Primary guardrail (in `crates/tests/`, real `run_with_transport` over
`LoopbackTransport`, mirroring `first_contact_direct.rs`):

- **`attachment_roundtrip_multichunk_over_loopback`** — Alice `SendFile`s a
  multi-chunk file (~700 KiB → ~15 chunks at 48 KiB) carrying real EXIF;
  Bob auto-fetches; assert `Event::AttachmentReceived`, the file at Bob's
  `download_dir` is **byte-identical to the stripped bytes**, and metadata is
  gone (EXIF absent).
- **`attachment_resume_after_reconnect`** — drop Bob's connection mid-transfer,
  `ReplaceConn`, assert the transfer completes and the file is byte-identical
  (proves in-session resume from `received_indices`).
- Unit coverage: frame codec round-trip for the 4 new types; filename
  sanitization (path traversal, control chars, length); window refill / retry
  budget / `ChunkNack` failure in `chunk_transfer`.

Success = both guardrails green through real production wiring, plus the full
workspace staying green under `cargo clippy -D warnings` / `cargo test` /
`cargo fmt --check`.
