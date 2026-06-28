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
        Self {
            manifest,
            needed,
            inflight: HashMap::new(),
            received,
            total,
        }
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
            let Some(idx) = self.needed.pop_front() else {
                break;
            };
            self.inflight.insert(
                idx,
                InFlight {
                    attempts: 1,
                    sent_at: Instant::now(),
                },
            );
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
        Ok(ct) => Frame::Chunk {
            attachment_id: *attachment_id,
            index,
            ciphertext: ct,
        },
        Err(_) => Frame::ChunkNack {
            attachment_id: *attachment_id,
            index,
            reason: NACK_STORE_READ,
        },
    }
}

/// Strip path separators, traversal, control chars, AND Unicode
/// bidi/format-control characters (the RTL-override display-spoofing vector,
/// e.g. U+202E) from an attacker-controlled manifest filename; cap length;
/// never empty.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_or_format_control(*c) && *c != '/' && *c != '\\')
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    let result = if trimmed.is_empty() { "file" } else { trimmed };
    result.chars().take(200).collect()
}

/// Unicode bidi/format controls that `char::is_control` (C0/C1 only) misses
/// but which enable filename-display spoofing. Mirrors the same check in
/// `attachment::manifest`. Covers bidi overrides/embeddings (U+202A–202E),
/// isolates (U+2066–2069), and LRM/RLM (U+200E/200F).
fn is_bidi_or_format_control(c: char) -> bool {
    matches!(c,
        '\u{200E}' | '\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attachment::manifest::{AttachmentManifest, ChunkRef};

    fn manifest_with(n: u32) -> AttachmentManifest {
        let chunks = (0..n)
            .map(|i| ChunkRef {
                index: i,
                ciphertext_hash: [i as u8; 32],
                len: 10,
            })
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
        for i in reqs {
            rx.on_received(i);
        }
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
    fn sanitize_strips_bidi_rtl_override() {
        // U+202E RIGHT-TO-LEFT OVERRIDE is the classic display-spoofing char
        // (e.g. "invoice\u{202E}cod.exe" renders as "invoiceexe.doc").
        let tricky = "invoice\u{202E}cod.exe";
        let result = sanitize_filename(tricky);
        assert!(
            !result.contains('\u{202E}'),
            "RTL override U+202E must be stripped; got: {result:?}"
        );
        // Other bidi controls must be stripped too.
        for ch in ['\u{200E}', '\u{200F}', '\u{202A}', '\u{2066}', '\u{2069}'] {
            let s = format!("file{ch}name.txt");
            let r = sanitize_filename(&s);
            assert!(
                !r.contains(ch),
                "bidi char U+{:04X} must be stripped; got: {r:?}",
                ch as u32
            );
        }
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
            Frame::Chunk {
                ciphertext, index, ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(ciphertext, b"chunk-bytes");
            }
            other => panic!("expected chunk, got {other:?}"),
        }
    }
}
