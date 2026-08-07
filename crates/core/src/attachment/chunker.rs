// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Pure chunker: plaintext → manifest + per-chunk ciphertext blobs.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::attachment::error_kind::AttachmentErrorKind;
use crate::attachment::manifest::{sanitize_filename, AttachmentManifest, ChunkRef};
use crate::attachment::{CHUNK_SIZE, MANIFEST_VERSION, MAX_ATTACHMENT_BYTES};
use crate::error::Result;
use crate::identity::derive::chunk_key_material;

/// Chunk + encrypt a plaintext buffer. Returns the manifest and the
/// index-ordered ciphertext chunks (caller stages them). Pure: randomness is
/// confined to `attachment_id` + `file_key`.
pub(crate) fn chunk_plaintext(
    plaintext: &[u8],
    filename: &str,
    mime: &str,
) -> Result<(AttachmentManifest, Vec<Vec<u8>>)> {
    chunk_plaintext_with_cap(plaintext, filename, mime, MAX_ATTACHMENT_BYTES)
}

pub(crate) fn chunk_plaintext_with_cap(
    plaintext: &[u8],
    filename: &str,
    mime: &str,
    max_bytes: u64,
) -> Result<(AttachmentManifest, Vec<Vec<u8>>)> {
    if plaintext.len() as u64 > max_bytes {
        return Err(AttachmentErrorKind::TooLarge.into());
    }
    let mut rng = rand::thread_rng();
    let mut attachment_id = [0u8; 16];
    rng.fill_bytes(&mut attachment_id);
    let mut file_key = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(file_key.as_mut());

    let mut chunks = Vec::new();
    let mut refs = Vec::new();
    for (i, plain) in plaintext.chunks(CHUNK_SIZE).enumerate() {
        let index = u32::try_from(i).map_err(|_| AttachmentErrorKind::TooLarge)?;
        let km = chunk_key_material(&file_key, index)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&km[..32]));
        let nonce = XNonce::from_slice(&km[32..56]);
        // No AAD: the per-index key already binds a chunk to its position (a
        // ciphertext only decrypts under its own index's key), and the manifest
        // ciphertext_hash + manifest-in-MLS bind it to the attachment. AAD would
        // be redundant.
        let ct = cipher
            .encrypt(nonce, plain)
            .map_err(|_| AttachmentErrorKind::AeadFailed)?;
        let hash: [u8; 32] = Sha256::digest(&ct).into();
        let len = u32::try_from(ct.len()).map_err(|_| AttachmentErrorKind::SizeMismatch)?;
        refs.push(ChunkRef {
            index,
            ciphertext_hash: hash,
            len,
        });
        chunks.push(ct);
    }

    let manifest = AttachmentManifest {
        manifest_version: MANIFEST_VERSION,
        attachment_id,
        filename: sanitize_filename(filename),
        mime: mime.to_string(),
        total_size: plaintext.len() as u64,
        chunk_size: CHUNK_SIZE as u32,
        file_key: *file_key,
        chunks: refs,
    };
    Ok((manifest, chunks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    #[test]
    fn chunks_plaintext_into_manifest_and_ciphertext() {
        let plaintext = vec![7u8; CHUNK_SIZE * 2 + 100]; // 2 full chunks + a partial
        let (manifest, chunks) =
            chunk_plaintext(&plaintext, "f.bin", "application/octet-stream").unwrap();
        assert_eq!(
            manifest.manifest_version,
            crate::attachment::MANIFEST_VERSION
        );
        assert_eq!(manifest.total_size, plaintext.len() as u64);
        assert_eq!(manifest.chunk_size, crate::attachment::CHUNK_SIZE as u32);
        assert_eq!(manifest.chunks.len(), 3);
        assert_eq!(chunks.len(), 3);
        for (i, ct) in chunks.iter().enumerate() {
            let h: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(ct).into();
            assert_eq!(manifest.chunks[i].ciphertext_hash, h);
            assert_eq!(manifest.chunks[i].len as usize, ct.len());
        }
    }

    #[test]
    fn chunker_rejects_oversize() {
        // Drive the cap path with a tiny cap so we don't allocate 100 MiB.
        let err = chunk_plaintext_with_cap(&[0u8; 8], "x", "y", 4).expect_err("must reject");
        assert!(matches!(
            err,
            CoreError::Attachment(AttachmentErrorKind::TooLarge)
        ));
    }

    #[test]
    fn empty_plaintext_yields_zero_chunks() {
        let (manifest, chunks) =
            chunk_plaintext(&[], "empty.bin", "application/octet-stream").unwrap();
        assert_eq!(manifest.total_size, 0);
        assert!(manifest.chunks.is_empty());
        assert!(chunks.is_empty());
        // a valid, version-stamped manifest is still produced
        assert_eq!(
            manifest.manifest_version,
            crate::attachment::MANIFEST_VERSION
        );
    }
}
