// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Attachment core (Phase 3.A): manifest format, chunker, reassembler,
//! metadata stripping. Pure/local — no transport. The manifest rides inside
//! MLS via `envelope::kinds::Kind::File`.

pub(crate) mod chunker;
pub(crate) mod error_kind;
pub(crate) mod manifest;
pub(crate) mod reassembler;
pub(crate) mod store;
pub(crate) mod strip;

pub(crate) use error_kind::AttachmentErrorKind;
pub(crate) use manifest::AttachmentManifest;

/// Plaintext bytes per chunk (48 KiB). Must stay under the Noise single-message
/// cap: `NOISE_MAX_OUTER = 65_519` bytes (65_535 − 16-byte ChaChaPoly tag).
/// A `Frame::Chunk` carrying `CHUNK_SIZE` plaintext + 16-byte AEAD tag encodes
/// to ~49 KiB CBOR, comfortably below 65_519. See
/// `transport::connection::AuthenticatedConnection::send` for the enforcement
/// point. The 3.C offline path (mailbox `Deposit`) has a 1 MiB cap which is
/// also satisfied.
pub(crate) const CHUNK_SIZE: usize = 49_152;

/// Maximum total plaintext attachment size (100 MiB), rejected up front.
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Files at/under this size may use the offline (mailbox) lane; larger files
/// are direct-only (3.B) — if the peer is offline they wait for both online.
pub(crate) const MAX_OFFLINE_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// How long after send the offline deposit rows become due — a head start for
/// the direct 3.B lane to complete first (and be pruned).
pub(crate) const OFFLINE_FALLBACK_STALL_SECS: i64 = 90;

/// Current manifest version. An unknown version is rejected on decode.
pub(crate) const MANIFEST_VERSION: u8 = 1;

/// Max chunk deposits attempted per sweep tick (≤ per_conn_deposits_per_min).
pub(crate) const CHUNK_SWEEP_BATCH: usize = 30;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    fn end_to_end_local_round_trip_through_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::attachment::store::ChunkStore::new(tmp.path());

        let original = vec![0xABu8; crate::attachment::CHUNK_SIZE + 12_345];
        // non-image bytes pass strip through unchanged
        let (stripped, mime) =
            crate::attachment::strip::strip_metadata(&original, "application/octet-stream")
                .unwrap();
        let (manifest, chunks) =
            crate::attachment::chunker::chunk_plaintext(&stripped, "blob.bin", &mime).unwrap();

        // stage ciphertext chunks
        for (i, ct) in chunks.iter().enumerate() {
            let index = u32::try_from(i).unwrap();
            store.put(&manifest.attachment_id, index, ct).unwrap();
        }
        // reassemble from the store
        let out = tmp.path().join("out.bin");
        let src = crate::attachment::store::StoreSource::new(&store, manifest.attachment_id);
        crate::attachment::reassembler::reassemble(&manifest, &src, &out).unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), original);

        // manifest survives a CBOR round-trip (as it would inside Kind::File):
        // full equality proves to_cbor/from_cbor preserve the file_key + every
        // per-chunk ChunkRef, not just the id/count.
        let back =
            crate::attachment::AttachmentManifest::from_cbor(&manifest.to_cbor().unwrap()).unwrap();
        assert_eq!(back, manifest);
    }
}
