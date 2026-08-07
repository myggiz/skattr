// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Pure reassembler: manifest + chunk source → verified plaintext file.

use std::io::Write;
use std::path::Path;

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use crate::attachment::error_kind::AttachmentErrorKind;
use crate::attachment::manifest::AttachmentManifest;
use crate::error::Result;
use crate::identity::derive::chunk_key_material;

/// Source of chunk ciphertext blobs by index (on-disk store in Task 6, or an
/// in-memory map in tests).
pub(crate) trait ChunkSource {
    fn get(&self, index: u32) -> Result<Vec<u8>>;
}

/// Verify + decrypt each chunk (hash check BEFORE decrypt), stream plaintext to
/// a temp file, then atomically rename to `output_path`. No partial output.
pub(crate) fn reassemble<S: ChunkSource>(
    manifest: &AttachmentManifest,
    source: &S,
    output_path: &Path,
) -> Result<()> {
    // Append ".part" rather than `with_extension` (which would *replace* the
    // output's extension, so `a.pdf` and `a.txt` would collide on `a.part`).
    let mut tmp_os = output_path.as_os_str().to_owned();
    tmp_os.push(".part");
    let tmp = std::path::PathBuf::from(tmp_os);
    // Validation-error paths (hash/AEAD/size) below remove `tmp`; the
    // `?`-propagating I/O errors (create/write/sync/rename) deliberately don't
    // — those are genuine disk failures, the `.part` is never mistaken for
    // output (rename only happens on full success), and a re-run truncates it.
    let mut out = std::fs::File::create(&tmp)?;
    let mut written: u64 = 0;
    for chunk_ref in &manifest.chunks {
        let ct = source.get(chunk_ref.index)?;
        let hash: [u8; 32] = Sha256::digest(&ct).into();
        if hash != chunk_ref.ciphertext_hash {
            let _ = std::fs::remove_file(&tmp);
            return Err(AttachmentErrorKind::ChunkHashMismatch.into());
        }
        let km = chunk_key_material(&manifest.file_key, chunk_ref.index)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&km[..32]));
        let nonce = XNonce::from_slice(&km[32..56]);
        let plain = match cipher.decrypt(nonce, ct.as_ref()) {
            Ok(p) => p,
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(AttachmentErrorKind::AeadFailed.into());
            }
        };
        out.write_all(&plain)?;
        written = written.saturating_add(plain.len() as u64);
    }
    out.sync_all()?;
    if written != manifest.total_size {
        let _ = std::fs::remove_file(&tmp);
        return Err(AttachmentErrorKind::SizeMismatch.into());
    }
    std::fs::rename(&tmp, output_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    struct MemSource(std::collections::HashMap<u32, Vec<u8>>);
    impl ChunkSource for MemSource {
        fn get(&self, index: u32) -> Result<Vec<u8>> {
            self.0
                .get(&index)
                .cloned()
                .ok_or_else(|| crate::attachment::AttachmentErrorKind::SizeMismatch.into())
        }
    }

    fn mem_source(chunks: Vec<Vec<u8>>) -> MemSource {
        MemSource(
            chunks
                .into_iter()
                .enumerate()
                .map(|(i, c)| (i as u32, c))
                .collect(),
        )
    }

    #[test]
    fn round_trips_byte_identical() {
        let plaintext = vec![9u8; crate::attachment::CHUNK_SIZE + 7];
        let (manifest, chunks) =
            crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
        let src = mem_source(chunks);
        let out = tempfile::NamedTempFile::new().unwrap();
        reassemble(&manifest, &src, out.path()).unwrap();
        assert_eq!(std::fs::read(out.path()).unwrap(), plaintext);
    }

    #[test]
    fn flipped_ciphertext_byte_fails_hash_check() {
        let plaintext = vec![1u8; 100];
        let (manifest, mut chunks) =
            crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
        chunks[0][0] ^= 0xFF; // corrupt before the hash
        let src = mem_source(chunks);
        let out = tempfile::NamedTempFile::new().unwrap();
        let err = reassemble(&manifest, &src, out.path()).expect_err("must reject");
        assert!(matches!(
            err,
            CoreError::Attachment(AttachmentErrorKind::ChunkHashMismatch)
        ));
    }

    #[test]
    fn flipped_tag_fails_aead() {
        let plaintext = vec![1u8; 100];
        let (mut manifest, mut chunks) =
            crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
        // Flip the last ciphertext byte (AEAD tag) AND update the manifest hash +
        // len so it passes the hash gate and fails at decrypt instead.
        let last = chunks[0].len() - 1;
        chunks[0][last] ^= 0xFF;
        manifest.chunks[0].ciphertext_hash =
            <sha2::Sha256 as sha2::Digest>::digest(&chunks[0]).into();
        manifest.chunks[0].len = chunks[0].len() as u32;
        let src = mem_source(chunks);
        let out = tempfile::NamedTempFile::new().unwrap();
        let err = reassemble(&manifest, &src, out.path()).expect_err("must reject");
        assert!(matches!(
            err,
            CoreError::Attachment(AttachmentErrorKind::AeadFailed)
        ));
    }
}
