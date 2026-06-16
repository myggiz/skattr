// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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
pub(crate) use manifest::{AttachmentManifest, ChunkRef};

/// Plaintext bytes per chunk (256 KiB). Sits under the mailbox 1 MiB
/// `max_deposit_size` (with AEAD + framing headroom) so a 3.C offline chunk
/// fits one `Deposit`.
pub(crate) const CHUNK_SIZE: usize = 262_144;

/// Maximum total plaintext attachment size (100 MiB), rejected up front.
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;

/// Current manifest version. An unknown version is rejected on decode.
pub(crate) const MANIFEST_VERSION: u8 = 1;

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
