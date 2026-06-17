# Phase 3.B — Direct Attachment Transfer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two online skattr daemons round-trip a multi-chunk file end-to-end through the real `run_with_transport` assembly over loopback — byte-identical, metadata stripped, driven by `Command::SendFile`, using pull/request-driven chunk delivery over the existing Noise channel.

**Architecture:** 3.A built the local pipeline (chunk → AEAD-seal → stage in `ChunkStore` → reassemble). 3.B adds four additive transport frame types (`0x0B`–`0x0E`), a `delivery::chunk_transfer` engine owned by the per-peer `delivery::peer` actor, and the send/receive orchestration. The manifest still travels in MLS via `Kind::File`; chunk blobs move as opaque, Noise-encrypted (not MLS-wrapped) frames. The receiver pulls indices it is missing; in-session resume falls out of 3.A's persisted `received_indices`.

**Tech Stack:** Rust 2021, Tokio, `tokio_util::codec`, `ciborium` (CBOR), `rusqlite`, OpenMLS (untouched here), `snow`/Noise (untouched here).

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-17-phase-3b-direct-attachment-transfer-design.md`. **ADR:** `docs/adr/0010-attachment-transport-frames.md`. Branch: `phase-3b-direct-attachment-transfer`.
- **Audit rule (defining):** every behavior must be proven through real `Daemon::run` / `run_with_transport` over loopback — NOT via `test_exports`. The guardrail tasks enforce this.
- **License header on every `.rs` file:** `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026 Myggiz AB` (GPLv3 for `core`/`cli`/`tests`).
- **No `unwrap()`/`expect()` in library code** (`crates/core`, `crates/cli`) — use `?` and typed errors. Tests may use them under the existing `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` on test modules.
- **Use `todo!()` never `unimplemented!()`.** All secret/key material zeroizes (3.A already handles `file_key` via `Zeroizing` in `chunk_key_material`).
- **Frame free bytes start at `0x0B`** (`0x0A` is `Error`). Decoder rejects unknown types — that is correct and load-bearing.
- **Wire serialization is CBOR via `ciborium`**; map ciborium errors with `.map_err(|e| CoreError::Frame(format!(...)))` (the `#[from]` is fragile — see CLAUDE.md).
- **Constants (3.B):** request window `CHUNK_WINDOW = 8`; per-index retry budget `CHUNK_RETRY_BUDGET = 3`; request timeout `CHUNK_REQUEST_TIMEOUT = 30s`. NACK reasons: `0` unknown attachment, `1` index out of range, `2` store read error.
- **Gate before any "done" claim:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` all green. Cargo isn't on PATH — prefix commands with `. "$HOME/.cargo/env" &&`.
- **Reviews go to opus** (crypto/protocol/transport change; second reviewer required per CLAUDE.md).
- **Out of scope:** 3.C offline mailbox-blob path, cross-session resume (daemon restart), 3.D UI, concurrent attachments per peer (>1 active), capability negotiation.

---

## File Structure

- **Create** `crates/core/src/delivery/chunk_transfer.rs` — the transfer engine: frame CBOR payload structs, `ChunkRx` receiver state machine (window/retry/verify/complete, IO-free), `serve_chunk_request` sender helper, `sanitize_filename`, `unique_download_path`, `AttachmentBegin`, and the `Nack*` reason constants. One responsibility: chunk-transfer logic, no transport/await.
- **Modify** `crates/core/src/transport/frame.rs` — add `FrameType::{ChunkRequest,Chunk,ChunkNack,AttachmentComplete}` (`0x0B`–`0x0E`), matching `Frame` variants, `frame_type()` arms, and codec encode/decode arms.
- **Modify** `crates/core/src/delivery/peer.rs` — `full_run` gains `chunk_store`/`download_dir` params; new `conn.recv` arms for the 4 frames; drain `take_begin_attachment` after `dispatch`; window refill; resume on `ReplaceConn`; timeout sweep on `retry_tick`. `InboundDispatch` gains `take_begin_attachment` + `attachment_received`/`attachment_progress`/`attachment_failed` (default no-ops).
- **Modify** `crates/core/src/delivery/hub.rs` — hub holds `Arc<ChunkStore>` + `download_dir`; thread both into `spawn_peer_actor` → `PeerConnection::spawn`.
- **Modify** `crates/core/src/delivery/mod.rs` — `mod chunk_transfer;`.
- **Modify** `crates/core/src/daemon/inbound.rs` — `DaemonInbound` gains a `begins` map; `Kind::File` ingest (validate → `AttachmentRepo` insert `direction='in'` → stash `AttachmentBegin`); implement the 4 new `InboundDispatch` methods (emit `Event::Attachment*`).
- **Modify** `crates/core/src/daemon/commands.rs` — `Command::SendFile`, `CommandResult::FileQueued`.
- **Modify** `crates/core/src/daemon/dispatch.rs` — `send_file` handler; route `Command::SendFile`.
- **Modify** `crates/core/src/daemon/events.rs` — `Event::{AttachmentReceived,AttachmentProgress,AttachmentFailed}`.
- **Modify** `crates/core/src/daemon/config.rs` — `Config::download_dir` (+ default `<data_dir>/downloads`); `apply_patch` honours new patch field.
- **Modify** `crates/core/src/daemon/commands.rs` (ConfigPatch) — `download_dir: Option<PathBuf>`.
- **Modify** `crates/core/src/daemon/state.rs` — pass `data_dir` + `config.download_dir` into the hub constructor.
- **Modify** `crates/cli/src/main.rs` — `send-file <contact> <path>` subcommand.
- **Create** `crates/tests/src/attachment_transfer_direct.rs` — the two guardrail tests; register in `crates/tests/src/lib.rs`.

---

## Task 1: Transport frame types `0x0B`–`0x0E`

**Files:**
- Modify: `crates/core/src/transport/frame.rs`

**Interfaces:**
- Produces: `Frame::ChunkRequest { attachment_id: [u8;16], index: u32 }`, `Frame::Chunk { attachment_id: [u8;16], index: u32, ciphertext: Vec<u8> }`, `Frame::ChunkNack { attachment_id: [u8;16], index: u32, reason: u8 }`, `Frame::AttachmentComplete { attachment_id: [u8;16] }`; `FrameType::{ChunkRequest=0x0B, Chunk=0x0C, ChunkNack=0x0D, AttachmentComplete=0x0E}`.

- [ ] **Step 1: Write failing round-trip tests.** Append to the `mod tests` block in `frame.rs`:

```rust
    #[test]
    fn decode_chunk_request_round_trips() {
        let f = Frame::ChunkRequest { attachment_id: [0x11; 16], index: 7 };
        match round_trip(f) {
            Frame::ChunkRequest { attachment_id, index } => {
                assert_eq!(attachment_id, [0x11; 16]);
                assert_eq!(index, 7);
            }
            other => panic!("expected ChunkRequest, got {other:?}"),
        }
    }

    #[test]
    fn decode_chunk_round_trips() {
        let f = Frame::Chunk { attachment_id: [0x22; 16], index: 3, ciphertext: vec![0xAB; 500] };
        match round_trip(f) {
            Frame::Chunk { attachment_id, index, ciphertext } => {
                assert_eq!(attachment_id, [0x22; 16]);
                assert_eq!(index, 3);
                assert_eq!(ciphertext, vec![0xAB; 500]);
            }
            other => panic!("expected Chunk, got {other:?}"),
        }
    }

    #[test]
    fn decode_chunk_nack_round_trips() {
        let f = Frame::ChunkNack { attachment_id: [0x33; 16], index: 1, reason: 2 };
        match round_trip(f) {
            Frame::ChunkNack { attachment_id, index, reason } => {
                assert_eq!(attachment_id, [0x33; 16]);
                assert_eq!(index, 1);
                assert_eq!(reason, 2);
            }
            other => panic!("expected ChunkNack, got {other:?}"),
        }
    }

    #[test]
    fn decode_attachment_complete_round_trips() {
        let f = Frame::AttachmentComplete { attachment_id: [0x44; 16] };
        match round_trip(f) {
            Frame::AttachmentComplete { attachment_id } => assert_eq!(attachment_id, [0x44; 16]),
            other => panic!("expected AttachmentComplete, got {other:?}"),
        }
    }

    #[test]
    fn chunk_request_uses_type_0x0b() {
        let buf = enc(Frame::ChunkRequest { attachment_id: [0; 16], index: 0 });
        assert_eq!(buf[4], 0x0B);
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib transport::frame 2>&1 | tail -20`
Expected: FAIL — `no variant named ChunkRequest`.

- [ ] **Step 3: Add the CBOR payload structs.** After the `ErrorPayload` struct (around line 26) add:

```rust
#[derive(Debug, Serialize, Deserialize)]
struct ChunkRequestPayload {
    attachment_id: [u8; 16],
    index: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChunkPayload {
    attachment_id: [u8; 16],
    index: u32,
    #[serde(with = "serde_bytes")]
    ciphertext: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChunkNackPayload {
    attachment_id: [u8; 16],
    index: u32,
    reason: u8,
}

#[derive(Debug, Serialize, Deserialize)]
struct AttachmentCompletePayload {
    attachment_id: [u8; 16],
}
```

(`serde_bytes` keeps the chunk ciphertext a compact CBOR byte string; it is already a workspace dependency used elsewhere in `core`. If `cargo` reports it missing for this crate, add `serde_bytes` to `crates/core/Cargo.toml` `[dependencies]` matching the version used in the lockfile.)

- [ ] **Step 4: Add the `FrameType` variants.** In `enum FrameType`, after `Error = 0x0A,`:

```rust
    /// Receiver→sender request for one attachment chunk by index.
    ChunkRequest = 0x0B,
    /// Sender→receiver chunk payload (opaque AEAD ciphertext).
    Chunk = 0x0C,
    /// Sender→receiver negative ack: cannot serve this index.
    ChunkNack = 0x0D,
    /// Receiver→sender: all chunks received + reassembled.
    AttachmentComplete = 0x0E,
```

- [ ] **Step 5: Add the `Frame` variants.** In `enum Frame`, after the `Error { code, message }` variant:

```rust
    /// Request one chunk of an attachment by index.
    ChunkRequest {
        /// 16-byte attachment id (from the manifest).
        attachment_id: [u8; 16],
        /// Zero-based chunk index.
        index: u32,
    },
    /// One attachment chunk: opaque AEAD ciphertext (NOT MLS-wrapped).
    Chunk {
        /// 16-byte attachment id.
        attachment_id: [u8; 16],
        /// Zero-based chunk index.
        index: u32,
        /// Opaque per-chunk ciphertext (verified against the manifest hash).
        ciphertext: Vec<u8>,
    },
    /// Negative ack: the sender cannot serve `index`.
    ChunkNack {
        /// 16-byte attachment id.
        attachment_id: [u8; 16],
        /// Zero-based chunk index.
        index: u32,
        /// Reason code: 0 unknown attachment, 1 index out of range, 2 store read error.
        reason: u8,
    },
    /// The receiver has all chunks and has reassembled the file.
    AttachmentComplete {
        /// 16-byte attachment id.
        attachment_id: [u8; 16],
    },
```

- [ ] **Step 6: Extend `frame_type()`.** Add to the match in `Frame::frame_type`:

```rust
            Frame::ChunkRequest { .. } => FrameType::ChunkRequest,
            Frame::Chunk { .. } => FrameType::Chunk,
            Frame::ChunkNack { .. } => FrameType::ChunkNack,
            Frame::AttachmentComplete { .. } => FrameType::AttachmentComplete,
```

- [ ] **Step 7: Extend the decoder.** In `Decoder::decode`'s `match type_byte`, before the `other =>` arm:

```rust
            0x0B => {
                let p: ChunkRequestPayload = ciborium::from_reader(&payload[..])
                    .map_err(|e| CoreError::Frame(format!("ChunkRequest CBOR: {e}")))?;
                Frame::ChunkRequest { attachment_id: p.attachment_id, index: p.index }
            }
            0x0C => {
                let p: ChunkPayload = ciborium::from_reader(&payload[..])
                    .map_err(|e| CoreError::Frame(format!("Chunk CBOR: {e}")))?;
                Frame::Chunk { attachment_id: p.attachment_id, index: p.index, ciphertext: p.ciphertext }
            }
            0x0D => {
                let p: ChunkNackPayload = ciborium::from_reader(&payload[..])
                    .map_err(|e| CoreError::Frame(format!("ChunkNack CBOR: {e}")))?;
                Frame::ChunkNack { attachment_id: p.attachment_id, index: p.index, reason: p.reason }
            }
            0x0E => {
                let p: AttachmentCompletePayload = ciborium::from_reader(&payload[..])
                    .map_err(|e| CoreError::Frame(format!("AttachmentComplete CBOR: {e}")))?;
                Frame::AttachmentComplete { attachment_id: p.attachment_id }
            }
```

- [ ] **Step 8: Extend the encoder.** In `Encoder::encode`'s `match item`, before the closing `}` of the match (alongside the `Error` arm):

```rust
            Frame::ChunkRequest { attachment_id, index } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&ChunkRequestPayload { attachment_id, index }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode ChunkRequest: {e}")))?;
                (0x0B, buf)
            }
            Frame::Chunk { attachment_id, index, ciphertext } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&ChunkPayload { attachment_id, index, ciphertext }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode Chunk: {e}")))?;
                (0x0C, buf)
            }
            Frame::ChunkNack { attachment_id, index, reason } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&ChunkNackPayload { attachment_id, index, reason }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode ChunkNack: {e}")))?;
                (0x0D, buf)
            }
            Frame::AttachmentComplete { attachment_id } => {
                let mut buf = Vec::new();
                ciborium::into_writer(&AttachmentCompletePayload { attachment_id }, &mut buf)
                    .map_err(|e| CoreError::Frame(format!("encode AttachmentComplete: {e}")))?;
                (0x0E, buf)
            }
```

- [ ] **Step 9: Run tests to verify they pass.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib transport::frame 2>&1 | tail -20`
Expected: PASS (all frame tests, old and new).

- [ ] **Step 10: Commit.**

```bash
git add crates/core/src/transport/frame.rs crates/core/Cargo.toml
git commit -m "feat(3.B): add chunk-transfer frame types 0x0B-0x0E"
```

---

## Task 2: `chunk_transfer` engine (IO-free receiver logic + helpers)

**Files:**
- Create: `crates/core/src/delivery/chunk_transfer.rs`
- Modify: `crates/core/src/delivery/mod.rs`

**Interfaces:**
- Consumes: `crate::attachment::AttachmentManifest`, `crate::attachment::store::ChunkStore` (Task references its `get_chunk`), `crate::transport::frame::Frame`.
- Produces:
  - `pub(crate) const CHUNK_WINDOW: usize = 8;` `CHUNK_RETRY_BUDGET: u8 = 3;` `CHUNK_REQUEST_TIMEOUT: Duration`.
  - `pub(crate) struct AttachmentBegin { pub attachment_id: [u8;16], pub manifest: AttachmentManifest }`
  - `pub(crate) struct ChunkRx` with: `new(manifest, already_received: &[u32]) -> Self`, `next_requests(&mut self) -> Vec<u32>`, `verify(&self, index: u32, ciphertext: &[u8]) -> bool`, `on_received(&mut self, index: u32) -> bool`, `on_bad(&mut self, index: u32) -> ChunkAction`, `timed_out(&mut self, now: Instant) -> ChunkAction`, `reissue(&self) -> Vec<u32>`, `is_complete(&self) -> bool`, `progress(&self) -> (u32,u32)`, `attachment_id(&self) -> [u8;16]`, `manifest(&self) -> &AttachmentManifest`.
  - `pub(crate) enum ChunkAction { Request(Vec<u32>), Fail }`
  - `pub(crate) fn serve_chunk_request(store, attachment_id, index, total_chunks) -> Frame`
  - `pub(crate) fn sanitize_filename(name: &str) -> String`
  - `pub(crate) fn unique_download_path(dir: &Path, filename: &str) -> PathBuf`
  - `pub(crate) const NACK_UNKNOWN_ATTACHMENT: u8 = 0; NACK_INDEX_OUT_OF_RANGE: u8 = 1; NACK_STORE_READ: u8 = 2;`

- [ ] **Step 1: Create the file with the failing test module.** Write `crates/core/src/delivery/chunk_transfer.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Direct attachment chunk-transfer engine (Phase 3.B).
//!
//! Pull/request-driven: the receiver ([`ChunkRx`]) requests the indices it is
//! missing; the sender serves them ([`serve_chunk_request`]). All logic here is
//! IO-free and synchronous so it can be unit-tested without a transport; the
//! per-peer actor in `delivery::peer` performs the actual frame send/recv and
//! the `ChunkStore` / `AttachmentRepo` IO.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::attachment::manifest::AttachmentManifest;
use crate::attachment::store::ChunkStore;
use crate::transport::frame::Frame;

/// Max chunk requests in flight at once (≈2 MiB at the 256 KiB chunk size).
pub(crate) const CHUNK_WINDOW: usize = 8;
/// Per-index retry budget before the transfer fails.
pub(crate) const CHUNK_RETRY_BUDGET: u8 = 3;
/// How long an outstanding request may go unanswered before re-request.
pub(crate) const CHUNK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// NACK reason codes.
pub(crate) const NACK_UNKNOWN_ATTACHMENT: u8 = 0;
pub(crate) const NACK_INDEX_OUT_OF_RANGE: u8 = 1;
pub(crate) const NACK_STORE_READ: u8 = 2;

/// A just-received inbound manifest the peer actor should begin fetching.
pub(crate) struct AttachmentBegin {
    pub attachment_id: [u8; 16],
    pub manifest: AttachmentManifest,
}

/// What the actor should do after a bad chunk / timeout.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChunkAction {
    /// Re-send requests for these indices.
    Request(Vec<u32>),
    /// Abort the transfer (retry budget exhausted / hard nack).
    Fail,
}

struct InFlight {
    attempts: u8,
    sent_at: Instant,
}

/// Receiver-side state machine for one inbound attachment. IO-free.
pub(crate) struct ChunkRx {
    manifest: AttachmentManifest,
    /// Missing indices not yet requested.
    needed: std::collections::VecDeque<u32>,
    /// Indices currently requested, awaiting a Chunk.
    inflight: HashMap<u32, InFlight>,
    received: u32,
    total: u32,
}

impl ChunkRx {
    pub(crate) fn new(manifest: AttachmentManifest, already_received: &[u32]) -> Self {
        let total = manifest.chunks.len() as u32;
        let have: std::collections::HashSet<u32> = already_received.iter().copied().collect();
        let needed: std::collections::VecDeque<u32> = manifest
            .chunks
            .iter()
            .map(|c| c.index)
            .filter(|i| !have.contains(i))
            .collect();
        let received = total - needed.len() as u32;
        Self { manifest, needed, inflight: HashMap::new(), received, total }
    }

    pub(crate) fn attachment_id(&self) -> [u8; 16] {
        self.manifest.attachment_id
    }

    pub(crate) fn manifest(&self) -> &AttachmentManifest {
        &self.manifest
    }

    /// Pull indices from `needed` until the in-flight window is full.
    /// Returns the indices that should be requested now.
    pub(crate) fn next_requests(&mut self) -> Vec<u32> {
        let mut out = Vec::new();
        while self.inflight.len() < CHUNK_WINDOW {
            let Some(idx) = self.needed.pop_front() else { break };
            self.inflight.insert(idx, InFlight { attempts: 1, sent_at: Instant::now() });
            out.push(idx);
        }
        out
    }

    /// True if `ciphertext` matches the manifest's recorded hash for `index`.
    pub(crate) fn verify(&self, index: u32, ciphertext: &[u8]) -> bool {
        let Some(c) = self.manifest.chunks.iter().find(|c| c.index == index) else {
            return false;
        };
        let hash: [u8; 32] = Sha256::digest(ciphertext).into();
        hash == c.ciphertext_hash
    }

    /// Record a verified chunk. Returns true if it was newly received.
    pub(crate) fn on_received(&mut self, index: u32) -> bool {
        if self.inflight.remove(&index).is_some() {
            self.received = self.received.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// A chunk failed (hash mismatch). Re-request unless the budget is spent.
    pub(crate) fn on_bad(&mut self, index: u32) -> ChunkAction {
        match self.inflight.get_mut(&index) {
            Some(f) if f.attempts < CHUNK_RETRY_BUDGET => {
                f.attempts += 1;
                f.sent_at = Instant::now();
                ChunkAction::Request(vec![index])
            }
            Some(_) => ChunkAction::Fail,
            None => ChunkAction::Request(vec![]), // already resolved; nothing to do
        }
    }

    /// Re-arm any requests that have been outstanding past the timeout.
    pub(crate) fn timed_out(&mut self, now: Instant) -> ChunkAction {
        let mut to_retry = Vec::new();
        for (idx, f) in self.inflight.iter_mut() {
            if now.duration_since(f.sent_at) >= CHUNK_REQUEST_TIMEOUT {
                if f.attempts >= CHUNK_RETRY_BUDGET {
                    return ChunkAction::Fail;
                }
                f.attempts += 1;
                f.sent_at = now;
                to_retry.push(*idx);
            }
        }
        ChunkAction::Request(to_retry)
    }

    /// Indices to re-request after a reconnect (all currently in flight).
    pub(crate) fn reissue(&self) -> Vec<u32> {
        self.inflight.keys().copied().collect()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.received >= self.total
    }

    pub(crate) fn progress(&self) -> (u32, u32) {
        (self.received, self.total)
    }
}

/// Serve one chunk request from the staged `ChunkStore`. Returns a `Chunk`
/// frame on success or a `ChunkNack` on out-of-range / read error.
pub(crate) fn serve_chunk_request(
    store: &ChunkStore,
    attachment_id: &[u8; 16],
    index: u32,
    total_chunks: u32,
) -> Frame {
    if index >= total_chunks {
        return Frame::ChunkNack {
            attachment_id: *attachment_id,
            index,
            reason: NACK_INDEX_OUT_OF_RANGE,
        };
    }
    match store.get_chunk(attachment_id, index) {
        Ok(ct) => Frame::Chunk { attachment_id: *attachment_id, index, ciphertext: ct },
        Err(_) => Frame::ChunkNack { attachment_id: *attachment_id, index, reason: NACK_STORE_READ },
    }
}

/// Strip path separators, traversal, and control chars from an
/// attacker-controlled manifest filename; cap length; never empty.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    let result = if trimmed.is_empty() { "file" } else { trimmed };
    result.chars().take(200).collect()
}

/// Return a non-colliding path under `dir` for `filename`, suffixing
/// ` (1)`, ` (2)`, ... before the extension on collision.
pub(crate) fn unique_download_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match filename.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (filename.to_string(), String::new()),
    };
    for n in 1..=9999 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(filename)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attachment::manifest::{AttachmentManifest, ChunkRef};

    fn manifest_with(n: u32) -> AttachmentManifest {
        let chunks = (0..n)
            .map(|i| ChunkRef { index: i, ciphertext_hash: [i as u8; 32], len: 10 })
            .collect();
        AttachmentManifest {
            manifest_version: 1,
            attachment_id: [0xAA; 16],
            filename: "f.bin".into(),
            mime: "application/octet-stream".into(),
            total_size: (n as u64) * 10,
            chunk_size: 10,
            file_key: [0; 32],
            chunks,
        }
    }

    #[test]
    fn next_requests_fills_window_to_eight() {
        let mut rx = ChunkRx::new(manifest_with(20), &[]);
        let first = rx.next_requests();
        assert_eq!(first.len(), CHUNK_WINDOW);
        // Window full → no more until something resolves.
        assert!(rx.next_requests().is_empty());
    }

    #[test]
    fn on_received_advances_and_refills() {
        let mut rx = ChunkRx::new(manifest_with(20), &[]);
        let first = rx.next_requests();
        assert!(rx.on_received(first[0]));
        let refill = rx.next_requests();
        assert_eq!(refill.len(), 1, "one slot freed → one new request");
        let (recv, total) = rx.progress();
        assert_eq!((recv, total), (1, 20));
    }

    #[test]
    fn new_skips_already_received_indices() {
        let mut rx = ChunkRx::new(manifest_with(10), &[0, 1, 2]);
        let (recv, total) = rx.progress();
        assert_eq!((recv, total), (3, 10));
        let reqs = rx.next_requests();
        assert!(!reqs.contains(&0) && !reqs.contains(&1) && !reqs.contains(&2));
    }

    #[test]
    fn on_bad_retries_until_budget_then_fails() {
        let mut rx = ChunkRx::new(manifest_with(1), &[]);
        let req = rx.next_requests();
        let idx = req[0];
        // attempts starts at 1; budget 3 → two retries, then Fail.
        assert_eq!(rx.on_bad(idx), ChunkAction::Request(vec![idx]));
        assert_eq!(rx.on_bad(idx), ChunkAction::Request(vec![idx]));
        assert_eq!(rx.on_bad(idx), ChunkAction::Fail);
    }

    #[test]
    fn is_complete_when_all_received() {
        let mut rx = ChunkRx::new(manifest_with(2), &[]);
        let reqs = rx.next_requests();
        for i in reqs { rx.on_received(i); }
        assert!(rx.is_complete());
    }

    #[test]
    fn sanitize_strips_traversal_and_separators() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_filename("  ..  "), "file");
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(sanitize_filename("clean.jpg"), "clean.jpg");
    }

    #[test]
    fn unique_path_suffixes_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = unique_download_path(dir.path(), "x.txt");
        std::fs::write(&p1, b"a").unwrap();
        let p2 = unique_download_path(dir.path(), "x.txt");
        assert!(p2.ends_with("x (1).txt"));
    }

    #[test]
    fn serve_request_out_of_range_nacks() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::new(dir.path());
        let f = serve_chunk_request(&store, &[0xAA; 16], 5, 3);
        match f {
            Frame::ChunkNack { reason, .. } => assert_eq!(reason, NACK_INDEX_OUT_OF_RANGE),
            other => panic!("expected nack, got {other:?}"),
        }
    }

    #[test]
    fn serve_request_returns_staged_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChunkStore::new(dir.path());
        store.put(&[0xAA; 16], 0, b"chunk-bytes").unwrap();
        match serve_chunk_request(&store, &[0xAA; 16], 0, 1) {
            Frame::Chunk { ciphertext, index, .. } => {
                assert_eq!(index, 0);
                assert_eq!(ciphertext, b"chunk-bytes");
            }
            other => panic!("expected chunk, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Register the module.** In `crates/core/src/delivery/mod.rs`, add alongside the other `mod` declarations:

```rust
pub(crate) mod chunk_transfer;
```

- [ ] **Step 3: Confirm `manifest`/`store` module paths are reachable.** `chunk_transfer` references `crate::attachment::manifest::AttachmentManifest`, `crate::attachment::manifest::ChunkRef`, and `crate::attachment::store::ChunkStore`. If those submodules are private to `attachment`, widen them to `pub(crate)` in `crates/core/src/attachment/mod.rs` (e.g. `pub(crate) mod manifest;` `pub(crate) mod store;`) — do NOT make them `pub`.

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -20`
Expected: compiles (fix any path visibility errors by widening to `pub(crate)` only).

- [ ] **Step 4: Run the tests to verify they pass.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib delivery::chunk_transfer 2>&1 | tail -25`
Expected: PASS (all 9 tests).

- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/delivery/chunk_transfer.rs crates/core/src/delivery/mod.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.B): chunk_transfer engine (ChunkRx, serve, sanitize)"
```

---

## Task 3: Events, config `download_dir`, ConfigPatch

**Files:**
- Modify: `crates/core/src/daemon/events.rs`
- Modify: `crates/core/src/daemon/config.rs`
- Modify: `crates/core/src/daemon/commands.rs` (the `ConfigPatch` struct)

**Interfaces:**
- Produces: `Event::AttachmentReceived { contact: PublicKey, attachment_id: Hex16, filename: String, mime: String, size: u64, path: String }`, `Event::AttachmentProgress { attachment_id: Hex16, received: u32, total: u32 }`, `Event::AttachmentFailed { attachment_id: Hex16, reason: String }`; `Config::download_dir: PathBuf`; `ConfigPatch::download_dir: Option<PathBuf>`.

- [ ] **Step 1: Write failing config tests.** Append to the `mod tests` in `config.rs`:

```rust
    #[test]
    fn download_dir_defaults_under_data_dir() {
        let mut c = Config::defaults().unwrap();
        c.data_dir = std::path::PathBuf::from("/tmp/skattr-x");
        assert_eq!(c.resolved_download_dir(), std::path::PathBuf::from("/tmp/skattr-x/downloads"));
    }

    #[test]
    fn apply_patch_sets_download_dir() {
        let mut c = Config::defaults().unwrap();
        let patch = crate::daemon::commands::ConfigPatch {
            download_dir: Some(std::path::PathBuf::from("/tmp/dl")),
            ..Default::default()
        };
        c.apply_patch(&patch).unwrap();
        assert_eq!(c.download_dir, Some(std::path::PathBuf::from("/tmp/dl")));
    }
```

- [ ] **Step 2: Run to verify failure.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::config 2>&1 | tail -15`
Expected: FAIL — `no field download_dir` / `no method resolved_download_dir`.

- [ ] **Step 3: Add the `Config` field.** In `struct Config` (after the `ui` field):

```rust
    /// Directory where received attachments are written. `None` → defaults to
    /// `<data_dir>/downloads`. New in 3.B.
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
```

- [ ] **Step 4: Add the resolver helper.** In `impl Config` add:

```rust
    /// The effective download directory: the configured `download_dir` or
    /// `<data_dir>/downloads` when unset.
    #[must_use]
    pub fn resolved_download_dir(&self) -> PathBuf {
        self.download_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("downloads"))
    }
```

Then update every `Config { .. }` literal / `Config::defaults` constructor in `config.rs` to initialise `download_dir: None` (the `#[serde(default)]` covers deserialization, but struct literals need the field). Search the file for where `Config` is built and add `download_dir: None,`.

- [ ] **Step 5: Add the `ConfigPatch` field.** In `struct ConfigPatch` (`commands.rs`), add:

```rust
    /// If `Some`, set the attachment download directory. New in 3.B.
    #[serde(default)]
    pub download_dir: Option<std::path::PathBuf>,
```

- [ ] **Step 6: Honour it in `apply_patch`.** In `Config::apply_patch`, before `Ok(())`:

```rust
        if let Some(d) = &patch.download_dir {
            self.download_dir = Some(d.clone());
        }
```

- [ ] **Step 7: Add the Event variants.** In `enum Event` (`events.rs`), after `LogRecord(...)`:

```rust
    /// An inbound attachment finished downloading + reassembling.
    AttachmentReceived {
        /// Sending peer.
        contact: PublicKey,
        /// 16-byte attachment id.
        attachment_id: crate::daemon::commands::Hex16,
        /// Sanitized filename as written to disk.
        filename: String,
        /// Effective MIME type (post-strip).
        mime: String,
        /// File size in bytes.
        size: u64,
        /// Absolute path of the written file.
        path: String,
    },
    /// Incremental attachment transfer progress (throttled).
    AttachmentProgress {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::commands::Hex16,
        /// Chunks received so far.
        received: u32,
        /// Total chunks.
        total: u32,
    },
    /// An attachment transfer failed (retry budget exhausted, hard nack).
    AttachmentFailed {
        /// 16-byte attachment id.
        attachment_id: crate::daemon::commands::Hex16,
        /// Human-readable, non-sensitive reason.
        reason: String,
    },
```

(Confirm `Hex16` is importable here — `commands.rs` defines it; the `Event` enum already references `commands` types like `MessageRecord`, so the path works.)

- [ ] **Step 8: Run config tests + build.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::config 2>&1 | tail -15 && cargo build -p skattr-core 2>&1 | tail -10`
Expected: config tests PASS; crate builds (the `ts_rs::TS` derive on `Event` re-exports the new variants — if `PathBuf` in `ConfigPatch` trips `ts_rs`, mirror the existing pattern: other `PathBuf` config fields are not in `ConfigPatch`, so add `#[ts(type = "string")]` to the new `download_dir` patch field).

- [ ] **Step 9: Commit.**

```bash
git add crates/core/src/daemon/events.rs crates/core/src/daemon/config.rs crates/core/src/daemon/commands.rs
git commit -m "feat(3.B): attachment events + download_dir config"
```

---

## Task 4: `InboundDispatch` attachment hooks + `DaemonInbound` `Kind::File` ingest

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (trait only)
- Modify: `crates/core/src/daemon/inbound.rs`

**Interfaces:**
- Produces (on `InboundDispatch`, all default no-ops):
  - `fn take_begin_attachment(&self, _peer: PublicKey) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> { None }`
  - `fn attachment_received(&self, _peer: PublicKey, _attachment_id: [u8;16], _filename: &str, _mime: &str, _size: u64, _path: &str) {}`
  - `fn attachment_progress(&self, _attachment_id: [u8;16], _received: u32, _total: u32) {}`
  - `fn attachment_failed(&self, _attachment_id: [u8;16], _reason: &str) {}`
- Consumes (in `DaemonInbound`): `AttachmentManifest::from_cbor`, `AttachmentRepo`, `sanitize_filename`, `MAX_ATTACHMENT_BYTES`.

- [ ] **Step 1: Add the trait methods.** In `crates/core/src/delivery/peer.rs`, inside `trait InboundDispatch`, after `dispatch_welcome_bootstrap`:

```rust
    /// Pop a queued inbound-attachment begin-request for `peer`, stashed when a
    /// `Kind::File` manifest was decrypted in `dispatch`. The actor calls this
    /// immediately after `dispatch` returns to learn it should start fetching.
    /// Default returns `None`.
    fn take_begin_attachment(
        &self,
        _peer: PublicKey,
    ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
        None
    }

    /// Emit `Event::AttachmentReceived`. Default no-op.
    fn attachment_received(
        &self,
        _peer: PublicKey,
        _attachment_id: [u8; 16],
        _filename: &str,
        _mime: &str,
        _size: u64,
        _path: &str,
    ) {
    }

    /// Emit `Event::AttachmentProgress`. Default no-op.
    fn attachment_progress(&self, _attachment_id: [u8; 16], _received: u32, _total: u32) {}

    /// Emit `Event::AttachmentFailed`. Default no-op.
    fn attachment_failed(&self, _attachment_id: [u8; 16], _reason: &str) {}
```

- [ ] **Step 2: Build to confirm the trait still compiles** (defaults mean existing impls are unaffected).

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -10`
Expected: compiles.

- [ ] **Step 3: Add the `begins` field to `DaemonInbound`.** In `struct DaemonInbound` add:

```rust
    /// Per-peer queue of inbound attachments whose manifest just arrived,
    /// awaiting the peer actor's `take_begin_attachment` drain.
    begins: std::sync::Mutex<
        std::collections::HashMap<PublicKey, std::collections::VecDeque<crate::delivery::chunk_transfer::AttachmentBegin>>,
    >,
```

And in `DaemonInbound::new`, add to the constructed `Self`:

```rust
            begins: std::sync::Mutex::new(std::collections::HashMap::new()),
```

- [ ] **Step 4: Handle `Kind::File` in `dispatch_for_group`.** In `inbound.rs`, immediately after the `ContactCardUpdate` fast-path block (before `// Capture MLS generation *after* decrypt`), insert:

```rust
    // --- Kind::File: stage the manifest + queue the chunk fetch (3.B). ---
    // The manifest message ALSO falls through to the normal persist below so
    // it appears in history and emits Event::MessageReceived; this block only
    // adds the attachment side-effects.
    if let crate::envelope::Kind::File { manifest: manifest_bytes } = &envelope.kind {
        match crate::attachment::manifest::AttachmentManifest::from_cbor(manifest_bytes) {
            Ok(m) if m.total_size <= crate::attachment::MAX_ATTACHMENT_BYTES
                && !m.chunks.is_empty() =>
            {
                let repo = crate::storage::attachments::AttachmentRepo::new(&self.pool);
                let total_chunks = m.chunks.len() as i64;
                if let Err(e) = repo.insert(
                    &m.attachment_id,
                    "in",
                    manifest_bytes,
                    total_chunks,
                    now_unix_seconds(),
                ) {
                    tracing::warn!(err = %e, "inbound: attachment manifest insert failed");
                } else {
                    let begin = crate::delivery::chunk_transfer::AttachmentBegin {
                        attachment_id: m.attachment_id,
                        manifest: m,
                    };
                    if let Ok(mut map) = self.begins.lock() {
                        map.entry(from).or_default().push_back(begin);
                    }
                }
            }
            Ok(_) => tracing::warn!("inbound: rejecting oversize/empty attachment manifest"),
            Err(e) => tracing::warn!(err = %e, "inbound: bad attachment manifest, skipping fetch"),
        }
    }
```

(`now_unix_seconds()` is already used in this function; `self.pool` is in scope. If `crate::attachment::MAX_ATTACHMENT_BYTES` is `pub(crate) const` in `attachment/mod.rs`, it's reachable; if not, widen its visibility to `pub(crate)`.)

- [ ] **Step 5: Implement the new `InboundDispatch` methods for `DaemonInbound`.** In the `impl InboundDispatch for DaemonInbound` block, add:

```rust
    fn take_begin_attachment(
        &self,
        peer: PublicKey,
    ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
        self.begins.lock().ok()?.get_mut(&peer)?.pop_front()
    }

    fn attachment_received(
        &self,
        contact: PublicKey,
        attachment_id: [u8; 16],
        filename: &str,
        mime: &str,
        size: u64,
        path: &str,
    ) {
        let _ = self.events_tx.send(Event::AttachmentReceived {
            contact,
            attachment_id: crate::daemon::commands::Hex16::from(attachment_id),
            filename: filename.to_string(),
            mime: mime.to_string(),
            size,
            path: path.to_string(),
        });
    }

    fn attachment_progress(&self, attachment_id: [u8; 16], received: u32, total: u32) {
        let _ = self.events_tx.send(Event::AttachmentProgress {
            attachment_id: crate::daemon::commands::Hex16::from(attachment_id),
            received,
            total,
        });
    }

    fn attachment_failed(&self, attachment_id: [u8; 16], reason: &str) {
        let _ = self.events_tx.send(Event::AttachmentFailed {
            attachment_id: crate::daemon::commands::Hex16::from(attachment_id),
            reason: reason.to_string(),
        });
    }
```

(Confirm `Hex16: From<[u8;16]>` — `MessageRecord::project` uses `Hex16::from(envelope.id.0)` where `.0` is `[u8;16]`, so the `From` impl exists.)

- [ ] **Step 6: Build + run the inbound unit tests.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib daemon::inbound 2>&1 | tail -20`
Expected: existing inbound tests still PASS; crate builds.

- [ ] **Step 7: Commit.**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/daemon/inbound.rs
git commit -m "feat(3.B): InboundDispatch attachment hooks + Kind::File ingest"
```

---

## Task 5: Peer actor — sender serve, receiver fetch, resume, events

**Files:**
- Modify: `crates/core/src/delivery/peer.rs`

**Interfaces:**
- Consumes: `ChunkRx`, `serve_chunk_request`, `sanitize_filename`, `unique_download_path`, `AttachmentBegin`, `ChunkAction`, `ChunkStore`, `StoreSource`, `reassemble`, `AttachmentRepo`.
- Produces: `full_run` gains two trailing params `chunk_store: Option<std::sync::Arc<crate::attachment::store::ChunkStore>>` and `download_dir: Option<std::path::PathBuf>`; `PeerConnection::spawn` gains the same two params.

> **Reviewer note:** this is the integration core. Keep IO calls (`ChunkStore`, `AttachmentRepo`, `reassemble`) inside the actor; keep window/retry decisions in `ChunkRx`. No `.await` may hold a `std::sync::Mutex` guard.

- [ ] **Step 1: Add the helper that drives a started transfer's requests.** Near the top of `peer.rs` (after the `use` block) add a small async helper so the `select!` arms stay readable:

```rust
/// Send `Frame::ChunkRequest` for each index over `conn`. Returns `false` if the
/// send failed (caller should drop the conn).
async fn send_chunk_requests<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    attachment_id: [u8; 16],
    indices: &[u32],
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Some(c) = conn.as_mut() else { return false };
    for &index in indices {
        if c.send(Frame::ChunkRequest { attachment_id, index }).await.is_err() {
            return false;
        }
    }
    true
}
```

- [ ] **Step 2: Extend `full_run` and `PeerConnection::spawn` signatures.** Add the two params (`chunk_store`, `download_dir`) to the end of both `full_run`'s parameter list and `PeerConnection::spawn`'s parameter list, and forward them from `spawn` into `full_run`. Add the local receiver state at the top of `full_run` (after `let fallback_enabled = ...;`):

```rust
    // 3.B inbound chunk-transfer state: at most one active inbound attachment
    // per peer; later begins queue FIFO. `None` chunk_store/download_dir
    // (test constructors) disables all chunk handling.
    let mut active_rx: Option<crate::delivery::chunk_transfer::ChunkRx> = None;
    let mut rx_queue: std::collections::VecDeque<
        crate::delivery::chunk_transfer::AttachmentBegin,
    > = std::collections::VecDeque::new();
    let chunk_enabled = chunk_store.is_some() && download_dir.is_some();
```

- [ ] **Step 3: Add an inline "start next inbound transfer" closure-free helper.** Because closures can't borrow `conn` mutably and async, inline this logic as a labelled block you call from the relevant arms. Add this private async fn to `peer.rs`:

```rust
/// Begin the next queued inbound attachment if none is active. Sends the first
/// request window. Returns the new `active_rx` (or `None` if nothing to start /
/// send failed). The caller assigns the result to its `active_rx`.
#[allow(clippy::too_many_arguments)]
async fn maybe_start_next_rx<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    pool: &std::sync::Arc<crate::storage::Pool>,
    rx_queue: &mut std::collections::VecDeque<crate::delivery::chunk_transfer::AttachmentBegin>,
) -> Option<crate::delivery::chunk_transfer::ChunkRx>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let begin = rx_queue.pop_front()?;
    let repo = crate::storage::attachments::AttachmentRepo::new(pool);
    let already = repo.received_indices(&begin.attachment_id).unwrap_or_default();
    let mut rx = crate::delivery::chunk_transfer::ChunkRx::new(begin.manifest, &already);
    if rx.is_complete() {
        // Already had everything (resume edge): caller will finalize on next tick.
        return Some(rx);
    }
    let reqs = rx.next_requests();
    let aid = rx.attachment_id();
    let _ = send_chunk_requests(conn, aid, &reqs).await;
    Some(rx)
}
```

- [ ] **Step 4: Drain `take_begin_attachment` in the `MlsApp` inbound arm.** In the `Ok(Some(Frame::MlsApp(ct)))` arm, after the existing `if let Some(mid) = d.dispatch(peer, &ct) { ...ACK... }` block and still inside `if let Some(d) = inbound.as_ref()`, add:

```rust
                            // 3.B: a Kind::File manifest may have queued a begin.
                            if chunk_enabled {
                                while let Some(begin) = d.take_begin_attachment(peer) {
                                    rx_queue.push_back(begin);
                                }
                                if active_rx.is_none() {
                                    active_rx = maybe_start_next_rx(&mut conn, &pool, &mut rx_queue).await;
                                }
                            }
```

- [ ] **Step 5: Add the sender-serve arm.** In the `conn.recv` `match frame`, add before `Ok(Some(other)) =>`:

```rust
                    Ok(Some(Frame::ChunkRequest { attachment_id, index })) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(store) = chunk_store.as_ref() {
                            let row = crate::storage::attachments::AttachmentRepo::new(&pool)
                                .get(&attachment_id)
                                .ok()
                                .flatten();
                            let total = row.as_ref().map(|r| r.total_chunks as u32).unwrap_or(0);
                            let reply = if row.is_none() {
                                Frame::ChunkNack {
                                    attachment_id,
                                    index,
                                    reason: crate::delivery::chunk_transfer::NACK_UNKNOWN_ATTACHMENT,
                                }
                            } else {
                                crate::delivery::chunk_transfer::serve_chunk_request(
                                    store, &attachment_id, index, total,
                                )
                            };
                            if let Some(c) = conn.as_mut() {
                                if c.send(reply).await.is_err() {
                                    conn = None;
                                    drain_pending(&mut pending);
                                }
                            }
                        }
                    }
```

- [ ] **Step 6: Add the receiver `Chunk` arm.** Add next:

```rust
                    Ok(Some(Frame::Chunk { attachment_id, index, ciphertext })) => {
                        last_traffic = tokio::time::Instant::now();
                        if chunk_enabled {
                            let matches = active_rx.as_ref().map(|r| r.attachment_id()) == Some(attachment_id);
                            if matches {
                                // Borrow split: take rx out, operate, put back.
                                let mut rx = active_rx.take().expect("matches implies Some");
                                if rx.verify(index, &ciphertext) {
                                    if let Some(store) = chunk_store.as_ref() {
                                        let _ = store.put(&attachment_id, index, &ciphertext);
                                    }
                                    let repo = crate::storage::attachments::AttachmentRepo::new(&pool);
                                    let _ = repo.mark_received(&attachment_id, index);
                                    rx.on_received(index);
                                    let (recv, total) = rx.progress();
                                    if let Some(d) = inbound.as_ref() {
                                        // Throttle: every 8th chunk or on completion.
                                        if recv % 8 == 0 || recv == total {
                                            d.attachment_progress(attachment_id, recv, total);
                                        }
                                    }
                                    if rx.is_complete() {
                                        finalize_rx(&mut conn, &pool, &inbound, &download_dir, &rx).await;
                                        active_rx = maybe_start_next_rx(&mut conn, &pool, &mut rx_queue).await;
                                    } else {
                                        let reqs = rx.next_requests();
                                        let aid = rx.attachment_id();
                                        let _ = send_chunk_requests(&mut conn, aid, &reqs).await;
                                        active_rx = Some(rx);
                                    }
                                } else {
                                    // Hash mismatch → retry or fail.
                                    match rx.on_bad(index) {
                                        crate::delivery::chunk_transfer::ChunkAction::Request(idxs) => {
                                            let aid = rx.attachment_id();
                                            let _ = send_chunk_requests(&mut conn, aid, &idxs).await;
                                            active_rx = Some(rx);
                                        }
                                        crate::delivery::chunk_transfer::ChunkAction::Fail => {
                                            fail_rx(&pool, &inbound, &rx, "chunk hash mismatch").await;
                                            active_rx = maybe_start_next_rx(&mut conn, &pool, &mut rx_queue).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Some(Frame::ChunkNack { attachment_id, index: _, reason })) => {
                        last_traffic = tokio::time::Instant::now();
                        if chunk_enabled
                            && active_rx.as_ref().map(|r| r.attachment_id()) == Some(attachment_id)
                        {
                            let rx = active_rx.take().expect("matches implies Some");
                            fail_rx(&pool, &inbound, &rx, &format!("sender nack reason {reason}")).await;
                            active_rx = maybe_start_next_rx(&mut conn, &pool, &mut rx_queue).await;
                        }
                    }
                    Ok(Some(Frame::AttachmentComplete { attachment_id })) => {
                        last_traffic = tokio::time::Instant::now();
                        // Sender side: receiver confirmed receipt → finalize the out row + GC.
                        let repo = crate::storage::attachments::AttachmentRepo::new(&pool);
                        let _ = repo.set_status(&attachment_id, "complete");
                        if let Some(store) = chunk_store.as_ref() {
                            let _ = store.remove(&attachment_id);
                        }
                        if let Some(d) = inbound.as_ref() {
                            if let Ok(Some(row)) = repo.get(&attachment_id) {
                                let t = row.total_chunks as u32;
                                d.attachment_progress(attachment_id, t, t);
                            }
                        }
                    }
```

- [ ] **Step 7: Add the `finalize_rx` and `fail_rx` helpers.** Add to `peer.rs`:

```rust
/// Reassemble a completed inbound attachment to the download dir and emit
/// `Event::AttachmentReceived` + send `AttachmentComplete` to the sender.
async fn finalize_rx<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    pool: &std::sync::Arc<crate::storage::Pool>,
    inbound: &Option<std::sync::Arc<dyn InboundDispatch>>,
    download_dir: &Option<std::path::PathBuf>,
    rx: &crate::delivery::chunk_transfer::ChunkRx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let manifest = rx.manifest();
    let aid = manifest.attachment_id;
    let Some(dir) = download_dir.as_ref() else { return };
    let _ = std::fs::create_dir_all(dir);
    let store = crate::attachment::store::ChunkStore::new(
        // ChunkStore root is <data_dir>/attachments; reuse the same data_dir
        // the actor's chunk_store points at by deriving from download_dir's
        // parent is WRONG — instead the actor passes a real ChunkStore. See note.
        dir, // placeholder — replaced in Step 8
    );
    let source = crate::attachment::store::StoreSource::new(&store, aid);
    let safe_name = crate::delivery::chunk_transfer::sanitize_filename(&manifest.filename);
    let out_path = crate::delivery::chunk_transfer::unique_download_path(dir, &safe_name);
    match crate::attachment::reassembler::reassemble(manifest, &source, &out_path) {
        Ok(()) => {
            let repo = crate::storage::attachments::AttachmentRepo::new(pool);
            let _ = repo.set_status(&aid, "complete");
            if let Some(d) = inbound.as_ref() {
                d.attachment_received(
                    PublicKey([0u8; 32]), // replaced with real peer in Step 8
                    aid,
                    &safe_name,
                    &manifest.mime,
                    manifest.total_size,
                    &out_path.to_string_lossy(),
                );
            }
            let _ = send_chunk_requests; // keep import warm
            if let Some(c) = conn.as_mut() {
                let _ = c.send(Frame::AttachmentComplete { attachment_id: aid }).await;
            }
        }
        Err(e) => {
            tracing::warn!(err = %e, "inbound: reassembly failed");
            fail_rx(pool, inbound, rx, "reassembly failed").await;
        }
    }
}

/// Mark an inbound transfer failed and emit `Event::AttachmentFailed`.
async fn fail_rx(
    pool: &std::sync::Arc<crate::storage::Pool>,
    inbound: &Option<std::sync::Arc<dyn InboundDispatch>>,
    rx: &crate::delivery::chunk_transfer::ChunkRx,
    reason: &str,
) {
    let aid = rx.attachment_id();
    let repo = crate::storage::attachments::AttachmentRepo::new(pool);
    let _ = repo.set_status(&aid, "failed");
    if let Some(d) = inbound.as_ref() {
        d.attachment_failed(aid, reason);
    }
}
```

> **Step 8 correction (apply before building):** `finalize_rx` needs the real `ChunkStore` and the real `peer`. Change `finalize_rx`'s signature to also take `chunk_store: &Option<std::sync::Arc<crate::attachment::store::ChunkStore>>` and `peer: PublicKey`; build `StoreSource` from that `ChunkStore` (deref the `Arc`), and pass `peer` to `attachment_received`. Update the two call sites in Steps 6 to `finalize_rx(&mut conn, &pool, &inbound, &chunk_store, &download_dir, peer, &rx).await;`. Remove the placeholder `ChunkStore::new(dir, ...)` and the `PublicKey([0u8;32])` placeholder. The `let _ = send_chunk_requests;` keep-warm line is then unnecessary — delete it.

- [ ] **Step 8: Wire resume into `ReplaceConn`.** In the `Some(PeerCtrl::ReplaceConn(new_conn))` arm, after `first_failure_at = None;`, add:

```rust
                        // 3.B: resume an in-flight inbound transfer on reconnect by
                        // re-issuing its outstanding requests over the fresh conn.
                        if let Some(rx) = active_rx.as_ref() {
                            let aid = rx.attachment_id();
                            let reqs = rx.reissue();
                            let _ = send_chunk_requests(&mut conn, aid, &reqs).await;
                        }
```

- [ ] **Step 9: Add the timeout sweep to the retry tick.** At the END of the `_ = retry_tick.tick() =>` arm (after the fallback block), add:

```rust
                if chunk_enabled {
                    if let Some(rx) = active_rx.as_mut() {
                        match rx.timed_out(tokio::time::Instant::now().into_std()) {
                            crate::delivery::chunk_transfer::ChunkAction::Request(idxs) if !idxs.is_empty() => {
                                let aid = rx.attachment_id();
                                let _ = send_chunk_requests(&mut conn, aid, &idxs).await;
                            }
                            crate::delivery::chunk_transfer::ChunkAction::Fail => {
                                let rx = active_rx.take().expect("as_mut implies Some");
                                fail_rx(&pool, &inbound, &rx, "request timeout").await;
                                active_rx = maybe_start_next_rx(&mut conn, &pool, &mut rx_queue).await;
                            }
                            _ => {}
                        }
                    }
                }
```

(`ChunkRx::timed_out` takes a `std::time::Instant`; `tokio::time::Instant::into_std()` converts. If `into_std` is unavailable under paused time, change `ChunkRx` to use `tokio::time::Instant` instead — but since this fn is also unit-tested with `std::time::Instant`, keep `std` and pass `std::time::Instant::now()` here instead of the tokio instant.)

> Simpler: in this arm use `std::time::Instant::now()` directly: `rx.timed_out(std::time::Instant::now())`.

- [ ] **Step 10: Fix all `full_run` / `spawn` call sites.** Add `None, None` (for `chunk_store, download_dir`) to: `PeerConnection::spawn` body's `full_run(...)` call; `spawn_full_for_test`'s `full_run(...)` call; `spawn`'s own new params (it must accept and forward them); and the three direct `super::full_run::<...>(...)` calls inside `peer.rs` `mod tests`. The hub (Task 6) passes real values via `PeerConnection::spawn`.

- [ ] **Step 11: Build + run all peer + chunk tests.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib delivery 2>&1 | tail -30`
Expected: all existing `delivery::peer` tests PASS (now compiled with `None, None`), `chunk_transfer` tests PASS.

- [ ] **Step 12: Commit.**

```bash
git add crates/core/src/delivery/peer.rs
git commit -m "feat(3.B): peer actor chunk serve/fetch/resume/timeout + events"
```

---

## Task 6: Hub plumbing for `ChunkStore` + `download_dir`

**Files:**
- Modify: `crates/core/src/delivery/hub.rs`
- Modify: `crates/core/src/daemon/state.rs`

**Interfaces:**
- Consumes: `full_run`/`PeerConnection::spawn` two new params (Task 5).
- Produces: `DeliveryHub` stores `chunk_store: Option<Arc<ChunkStore>>` + `download_dir: Option<PathBuf>`; `new_with_inbound_dialer_and_fallback` gains a `data_dir: &Path` + `download_dir: PathBuf` (or accepts `&Config`); other constructors default both to `None`.

- [ ] **Step 1: Add hub fields.** In `struct DeliveryHub`, add:

```rust
    /// Staged-chunk store for attachment transfer (3.B). `None` disables.
    chunk_store: Option<Arc<crate::attachment::store::ChunkStore>>,
    /// Where reassembled inbound attachments are written. `None` disables.
    download_dir: Option<std::path::PathBuf>,
```

- [ ] **Step 2: Thread through `new_inner`.** Add two params `chunk_store: Option<Arc<...ChunkStore>>` and `download_dir: Option<PathBuf>` to `new_inner`; set them on `Self`. Update every constructor that calls `new_inner` to pass `None, None` EXCEPT the production one (next step). Each non-production constructor (`new`, `new_with_inbound`, `new_with_inbound_and_dialer`, `new_with_dialer`, `new_with_mailbox_fallback`) passes `None, None`.

- [ ] **Step 3: Production constructor takes the dirs.** Change `new_with_inbound_dialer_and_fallback` to accept `data_dir: &std::path::Path` and `download_dir: std::path::PathBuf`, build `let chunk_store = Some(Arc::new(crate::attachment::store::ChunkStore::new(data_dir)));`, and forward `chunk_store, Some(download_dir)` into `new_inner`.

- [ ] **Step 4: Pass the dirs into the actor.** In `spawn_peer_actor`, extend the `PeerConnection::spawn::<S>(...)` call with the two trailing args:

```rust
            self.chunk_store.clone(),
            self.download_dir.clone(),
```

- [ ] **Step 5: Update `run_with_transport`.** In `crates/core/src/daemon/state.rs`, where `DeliveryHub::new_with_inbound_dialer_and_fallback(...)` is constructed (around line 317), pass `data_dir` and the resolved download dir. The config is in scope as `config`:

```rust
            data_dir,
            config.resolved_download_dir(),
```

(Match the exact arg order of the new signature. `data_dir: &Path` is already a `run_with_transport` parameter.)

- [ ] **Step 6: Build the whole workspace.**

Run: `. "$HOME/.cargo/env" && cargo build --workspace 2>&1 | tail -20`
Expected: compiles. Fix any constructor call sites the compiler flags (tests in `hub.rs`/integration tests that call the changed constructors — pass `None, None` or the dirs as appropriate).

- [ ] **Step 7: Run delivery + daemon tests.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib delivery 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 8: Commit.**

```bash
git add crates/core/src/delivery/hub.rs crates/core/src/daemon/state.rs
git commit -m "feat(3.B): hub threads ChunkStore + download_dir into peer actors"
```

---

## Task 7: `Command::SendFile` + handler + CLI

**Files:**
- Modify: `crates/core/src/daemon/commands.rs`
- Modify: `crates/core/src/daemon/dispatch.rs`
- Modify: `crates/cli/src/main.rs`

**Interfaces:**
- Consumes: `send_message` (existing handler, reused for the manifest), `ChunkStore`, `AttachmentRepo`, `chunk_plaintext`, `strip_metadata`, `AttachmentManifest::to_cbor`.
- Produces: `Command::SendFile { contact: PublicKey, path: String }`; `CommandResult::FileQueued { message_id: Hex16, attachment_id: Hex16, total_chunks: u32 }`.

- [ ] **Step 1: Add the `Command` variant.** In `enum Command`, after `SendMessage { .. }`:

```rust
    /// Send a file attachment to a contact. Strips metadata, chunks, stages,
    /// persists the manifest, and announces it via a `Kind::File` MLS message;
    /// chunk bytes transfer pull-driven over the direct transport (3.B).
    SendFile {
        /// Recipient identity pubkey.
        contact: PublicKey,
        /// Local filesystem path of the file to send.
        path: String,
    },
```

- [ ] **Step 2: Add the `CommandResult` variant.** In `enum CommandResult`, after `MessageSent { .. }`:

```rust
    /// A file attachment was staged + its manifest announced.
    FileQueued {
        /// Message id of the `Kind::File` manifest message.
        message_id: Hex16,
        /// 16-byte attachment id.
        attachment_id: Hex16,
        /// Number of chunks staged.
        total_chunks: u32,
    },
```

- [ ] **Step 3: Route the command.** In `execute_command` (`dispatch.rs`), add to the `match cmd`:

```rust
        Command::SendFile { contact, path } => send_file(&handle, contact, path).await,
```

- [ ] **Step 4: Write the `send_file` handler.** Add to `dispatch.rs`:

```rust
async fn send_file<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    path: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 1. Read the file (cap before chunking happens inside chunk_plaintext).
    let raw = std::fs::read(&path)
        .map_err(|e| IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            message: format!("cannot read file: {e}"),
        }))?;
    let filename = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let guessed_mime = "application/octet-stream";

    // 2. Strip metadata, 3. chunk (chunk_plaintext enforces MAX_ATTACHMENT_BYTES).
    let (stripped, mime) = crate::attachment::strip::strip_metadata(&raw, guessed_mime)
        .map_err(map_err)?;
    let (manifest, ciphertexts) =
        crate::attachment::chunker::chunk_plaintext(&stripped, &filename, &mime)
            .map_err(map_err)?;

    // 4. Stage every chunk in the ChunkStore.
    let data_dir = {
        let cfg = handle.config.read().await;
        cfg.data_dir.clone()
    };
    let store = crate::attachment::store::ChunkStore::new(&data_dir);
    for (i, ct) in ciphertexts.iter().enumerate() {
        store.put(&manifest.attachment_id, i as u32, ct).map_err(map_err)?;
    }

    // 5. Persist the out-row.
    let manifest_cbor = manifest.to_cbor().map_err(map_err)?;
    let total_chunks = manifest.chunks.len() as u32;
    let repo = crate::storage::attachments::AttachmentRepo::new(&handle.pool);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    repo.insert(&manifest.attachment_id, "out", &manifest_cbor, total_chunks as i64, now)
        .map_err(map_err)?;

    // 6. Announce via the existing MLS send path (Kind::File).
    let kind = crate::envelope::Kind::File { manifest: manifest_cbor };
    let sent = send_message(handle, contact, kind).await?;
    let message_id = match sent {
        CommandResult::MessageSent { message_id, .. } => message_id,
        other => return Ok(other),
    };

    Ok(CommandResult::FileQueued {
        message_id,
        attachment_id: Hex16::from(manifest.attachment_id),
        total_chunks,
    })
}
```

(Confirm `chunker`/`strip`/`store` are reachable as `pub(crate)` from `crate::attachment::*`; widen if needed. `map_err` is the existing helper used by `send_message`.)

- [ ] **Step 5: Build core.**

Run: `. "$HOME/.cargo/env" && cargo build -p skattr-core 2>&1 | tail -15`
Expected: compiles.

- [ ] **Step 6: Add the CLI subcommand.** In `crates/cli/src/main.rs`, add a `SendFile { contact: String, path: String }` arm to the clap `Command` enum (mirroring the existing `Send` subcommand), and in its handler:

```rust
    let result = match client
        .execute(CoreCommand::SendFile { contact: pubkey, path: path.clone() })
        .await
    {
        Ok(r) => r,
        Err(e) => exit_on_ipc_error(e),
    };
    match result {
        CommandResult::FileQueued { message_id, attachment_id, total_chunks } => {
            println!("{message_id}  file queued  attachment={attachment_id}  chunks={total_chunks}");
        }
        other => anyhow::bail!("unexpected result: {other:?}"),
    }
```

(Resolve `pubkey` from `contact` using the same contact-resolution helper the existing `Send` arm uses.)

- [ ] **Step 7: Build the workspace + clippy.**

Run: `. "$HOME/.cargo/env" && cargo build --workspace 2>&1 | tail -15 && cargo clippy -p skattr-core -p skattr-cli --all-targets -- -D warnings 2>&1 | tail -15`
Expected: compiles, no clippy warnings.

- [ ] **Step 8: Commit.**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs crates/cli/src/main.rs
git commit -m "feat(3.B): Command::SendFile handler + CLI send-file"
```

---

## Task 8: Guardrail — multi-chunk round-trip over loopback

**Files:**
- Create: `crates/tests/src/attachment_transfer_direct.rs`
- Modify: `crates/tests/src/lib.rs`

**Interfaces:**
- Consumes: `loopback_harness::{init_vault, config_for, PASSPHRASE}`, `run_loopback`, `LoopbackNet`, `IpcClient`, `Command`, `CommandResult`, `Event`, `EventFilter`.

> The test mirrors `first_contact_direct.rs`. It must drive `run_loopback` (real `run_with_transport`), NOT `test_exports`.

- [ ] **Step 1: Write the failing round-trip test.** Create `crates/tests/src/attachment_transfer_direct.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.B guardrail: a multi-chunk file round-trips byte-identically
//! between two real daemons over loopback, metadata stripped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use skattr_core::daemon::commands::{Command, CommandResult};
use skattr_core::daemon::events::{Event, EventFilter};
use skattr_core::daemon::ipc::IpcClient;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::loopback_harness::{config_for, init_vault, PASSPHRASE};
use crate::loopback_net::LoopbackNet;

/// Build a ~700 KiB JPEG-with-EXIF payload (3 chunks at 256 KiB) so we exercise
/// both multi-chunk transfer AND metadata stripping.
fn jpeg_with_exif(total_len: usize) -> Vec<u8> {
    // Minimal JPEG: SOI + APP1(EXIF) + filler + EOI. The strip step must remove
    // APP1; the reassembled file must equal strip_metadata(payload).
    let mut v = vec![0xFF, 0xD8]; // SOI
    // APP1 EXIF marker with a tiny payload.
    v.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x10]);
    v.extend_from_slice(b"Exif\0\0");
    v.extend_from_slice(&[0u8; 8]);
    // Filler scan data up to total_len, then EOI.
    while v.len() < total_len.saturating_sub(2) {
        v.push(0x55);
    }
    v.extend_from_slice(&[0xFF, 0xD9]); // EOI
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_roundtrip_multichunk_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let net = LoopbackNet::new();
    let pw = Zeroizing::new(PASSPHRASE.to_string());

    // Spawn Alice + Bob (identical to first_contact_direct).
    let (ready_a_tx, ready_a_rx) = oneshot::channel();
    let (shutdown_a_tx, shutdown_a_rx) = oneshot::channel::<()>();
    let a_dir = tmp_a.path().to_path_buf();
    let a_cfg = config_for(tmp_a.path());
    let (a_net, a_pw) = (net.clone(), pw.clone());
    let task_a = tokio::spawn(async move {
        skattr_core::test_support::run_loopback(
            &a_dir, &a_pw, a_cfg, std::path::PathBuf::from("/dev/null"),
            a_net, "alice.onion".into(), ready_a_tx,
            async move { let _ = shutdown_a_rx.await; },
        ).await
    });
    let (ready_b_tx, ready_b_rx) = oneshot::channel();
    let (shutdown_b_tx, shutdown_b_rx) = oneshot::channel::<()>();
    let b_dir = tmp_b.path().to_path_buf();
    let b_cfg = config_for(tmp_b.path());
    let (b_net, b_pw) = (net.clone(), pw.clone());
    let task_b = tokio::spawn(async move {
        skattr_core::test_support::run_loopback(
            &b_dir, &b_pw, b_cfg, std::path::PathBuf::from("/dev/null"),
            b_net, "bob.onion".into(), ready_b_tx,
            async move { let _ = shutdown_b_rx.await; },
        ).await
    });

    let ready_a = tokio::time::timeout(Duration::from_secs(60), ready_a_rx).await.unwrap().unwrap();
    let ready_b = tokio::time::timeout(Duration::from_secs(60), ready_b_rx).await.unwrap().unwrap();

    // Invite + add (Alice invites, Bob adds), then wait Active.
    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let invite_url = match client_a.execute(Command::CreateInvite { nickname: None, ttl_secs: Some(600) }).await.unwrap() {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let alice_pubkey = match client_b.execute(Command::AddContact { invite_url }).await.unwrap() {
        CommandResult::ContactAdded(s) => s.pubkey,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    let bob_pubkey = match IpcClient::connect(&ready_b.ipc_socket).await.unwrap()
        .execute(Command::DaemonInfo).await.unwrap()
    {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };
    crate::loopback_harness::wait_for_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(30)).await;

    // Write the source file on Alice's side.
    let payload = jpeg_with_exif(700 * 1024);
    let src = tmp_a.path().join("photo.jpg");
    std::fs::write(&src, &payload).unwrap();
    // Expected received bytes = the stripped payload.
    let (expected, _mime) = skattr_core::attachment::strip_for_test(&payload, "image/jpeg");

    // Subscribe Bob to attachment events BEFORE sending.
    let mut bob_sub = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    bob_sub.subscribe(EventFilter::All).await.unwrap();

    // Alice sends the file.
    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    match send_a.execute(Command::SendFile { contact: bob_pubkey, path: src.to_string_lossy().to_string() }).await.unwrap() {
        CommandResult::FileQueued { total_chunks, .. } => assert!(total_chunks >= 3),
        other => panic!("expected FileQueued, got {other:?}"),
    }

    // Await AttachmentReceived on Bob.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let written_path = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "AttachmentReceived not seen");
        if let Ok(Ok(ev)) = tokio::time::timeout(remaining, bob_sub.next_event()).await {
            if let Event::AttachmentReceived { contact, path, .. } = ev {
                assert_eq!(contact, alice_pubkey);
                break path;
            }
        }
    };

    // Byte-identical + metadata stripped.
    let got = std::fs::read(&written_path).unwrap();
    assert_eq!(got, expected, "received file must equal stripped source");
    assert!(!contains_exif_app1(&got), "EXIF APP1 must be stripped");

    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}

fn contains_exif_app1(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == [0xFF, 0xE1])
}
```

> **Note on test seams:** this test references `skattr_core::test_support::run_loopback`, `skattr_core::attachment::strip_for_test`, and `EventFilter::All`. Before writing the test, confirm the exact public path of `run_loopback` used by `first_contact_direct.rs` (copy its `use`/call verbatim — it may be `crate::loopback_net::run_loopback` or under `test_exports`). For the expected-bytes comparison, if no public strip helper exists, instead assert the received file is a valid JPEG that no longer contains `[0xFF,0xE1]` AND has length `< payload.len()` (strip removed APP1), rather than exact-equality against a private strip fn. Use whichever event filter `first_contact_direct.rs` uses (`EventFilter::Messages` won't carry attachment events — use the broadest filter available; if only `Messages` exists, widen the filter enum is OUT of scope, so subscribe via the same mechanism the harness exposes and match `Event::AttachmentReceived`).

- [ ] **Step 2: Register the module.** In `crates/tests/src/lib.rs`, alongside `mod first_contact_direct;`:

```rust
#[cfg(test)]
mod attachment_transfer_direct;
```

- [ ] **Step 3: Run the round-trip guardrail.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests attachment_roundtrip_multichunk_over_loopback -- --nocapture 2>&1 | tail -40`
Expected: PASS (Bob receives the file, byte-check holds, EXIF gone).

- [ ] **Step 4: Commit.**

```bash
git add crates/tests/src/attachment_transfer_direct.rs crates/tests/src/lib.rs
git commit -m "test(3.B): multi-chunk attachment round-trip guardrail over loopback"
```

---

## Task 9: Guardrail — in-session resume after reconnect

**Files:**
- Modify: `crates/tests/src/attachment_transfer_direct.rs`

**Interfaces:**
- Consumes: the same harness; forces a mid-transfer reconnect (drop + re-dial) and asserts completion.

- [ ] **Step 1: Write the failing resume test.** Append to `attachment_transfer_direct.rs` a second `#[tokio::test]` `attachment_resume_after_reconnect`. Reuse the setup from Task 8 up to the `SendFile`. To force a reconnect mid-transfer, the simplest deterministic lever available through the public surface is to send the file, then immediately trigger a fresh inbound dial from Alice to Bob (e.g. Alice sends a `Kind::Text` message, which `ingest`s a connection and fires `PeerCtrl::ReplaceConn` on Bob's actor — exercising the resume path), then assert `AttachmentReceived` still arrives:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_resume_after_reconnect() {
    // ... identical setup through wait_for_group_active + write source file ...
    // Send the file:
    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let _ = send_a.execute(Command::SendFile {
        contact: bob_pubkey,
        path: src.to_string_lossy().to_string(),
    }).await.unwrap();

    // Force a fresh connection mid-transfer: a text message re-ingests a conn,
    // firing ReplaceConn on Bob's peer actor → resume path re-issues requests.
    let _ = send_a.execute(Command::SendMessage {
        contact: bob_pubkey,
        kind: skattr_core::envelope::Kind::Text { body: "ping".into() },
    }).await.unwrap();

    // Despite the reconnect, the transfer completes.
    // ... same AttachmentReceived await + byte-identical assertion as Task 8 ...
}
```

Factor the shared setup (spawn, invite, add, wait-active, write source, expected bytes) into a private async helper `async fn setup_pair_and_send(...)` in the test file so both tests reuse it (DRY). The resume test's distinguishing assertion is identical (byte-identical received file) — the point is it survives the extra reconnect.

- [ ] **Step 2: Run the resume guardrail.**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests attachment_resume_after_reconnect -- --nocapture 2>&1 | tail -40`
Expected: PASS.

- [ ] **Step 3: Commit.**

```bash
git add crates/tests/src/attachment_transfer_direct.rs
git commit -m "test(3.B): in-session resume after reconnect guardrail"
```

---

## Task 10: Full-gate verification + docs

**Files:**
- Modify: `CLAUDE.md` (Phase 3 status), `PICKUP.md` (next workstream), `docs/adr/0010-attachment-transport-frames.md` (Status → Accepted)

- [ ] **Step 1: Run the full gate.**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace 2>&1 | tail -40
```
Expected: fmt clean; zero clippy warnings; all tests PASS (the two new guardrails included; real-Tor tests remain `--ignored`).

- [ ] **Step 2: Run cargo-deny** (no new deps expected unless `serde_bytes` was added):

Run: `. "$HOME/.cargo/env" && cargo deny check 2>&1 | tail -15`
Expected: ok.

- [ ] **Step 3: Update docs.** In `CLAUDE.md` mark **3.B done, 3.C next** (mirror the 3.A entry style with the merge commit once merged); in `PICKUP.md` set the next workstream to **3.C (offline transfer)**; flip ADR 0010 `Status: Proposed` → `Accepted`.

- [ ] **Step 4: Commit.**

```bash
git add CLAUDE.md PICKUP.md docs/adr/0010-attachment-transport-frames.md
git commit -m "docs(3.B): mark Phase 3.B done, 3.C next; accept ADR 0010"
```

- [ ] **Step 5: Finish the branch.** Use `superpowers:finishing-a-development-branch` to decide merge into `master` (the repo keeps `master` local-only per PICKUP).

---

## Self-Review

**Spec coverage** (each spec section → task):
- §2.1 pull/request-driven → Tasks 1, 2, 5. §2.2 auto-fetch + offer on event → Tasks 4, 5 (`AttachmentReceived` carries filename/mime/size/path). §2.3 `download_dir` + sanitize + collision → Tasks 2 (`sanitize_filename`/`unique_download_path`), 3 (config). §2.4 window N=8 / one-at-a-time / FIFO → Task 2 (`CHUNK_WINDOW`, `ChunkRx`), Task 5 (`active_rx` + `rx_queue`). §2.5 in-session resume → Task 5 Step 8 + Task 9. §2.6 throttled progress → Task 5 Step 6 (every 8th chunk / completion).
- §3 four frame types `0x0B`–`0x0E` → Task 1. §4 `chunk_transfer` engine → Task 2. §5 send path → Task 7. §6 receive path → Tasks 4 + 5. §7 errors (hash retry/budget 3, nack→fail, timeout 30s) → Task 2 (`on_bad`/`timed_out`) + Task 5. §8 events + config → Task 3. §9 assembly touchpoints → Tasks 4–7. §10 deferrals → respected (no mailbox/cross-session/concurrent paths added). §11 guardrails → Tasks 8 + 9.

**Placeholder scan:** Task 5 Step 7 deliberately ships a *wrong* placeholder (`ChunkStore::new(dir,...)` + `PublicKey([0;32])`) that Step 8 corrects — this is a flagged two-step refactor, not an unresolved placeholder; the corrected signature is fully specified. Task 8's "Note on test seams" leaves the exact `run_loopback` path to be copied verbatim from `first_contact_direct.rs` because that path wasn't quoted in full during extraction — the implementer copies the real `use`/call rather than guessing. No other TBDs.

**Type consistency:** `ChunkRx` method names (`new`, `next_requests`, `verify`, `on_received`, `on_bad`, `timed_out`, `reissue`, `is_complete`, `progress`, `attachment_id`, `manifest`) are used identically in Tasks 2 and 5. `Frame` variant field names (`attachment_id`, `index`, `ciphertext`, `reason`) match between Task 1 (definition) and Tasks 5/2 (use). `Hex16::from([u8;16])`, `AttachmentRepo::{new,insert,get,mark_received,received_indices,set_status}`, `ChunkStore::{new,put,get_chunk,remove}`, `StoreSource::new`, `reassemble`, `chunk_plaintext`, `strip_metadata`, `AttachmentManifest::{to_cbor,from_cbor}` all match the signatures extracted from the current tree.
