// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

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
    // The temp file holds DECRYPTED plaintext of an attachment that is
    // otherwise kept encrypted at rest, so no failure may leave it behind —
    // not a validation error, not a disk error, not a panic (#156). An
    // earlier version cleaned up only the validation paths on the grounds
    // that a stray `.part` is never mistaken for real output; true, but it
    // answers a correctness question rather than the security one.
    let cleanup = crate::on_drop::OnDrop::new({
        let tmp = tmp.clone();
        move || {
            let _ = std::fs::remove_file(&tmp);
        }
    });
    let mut out = std::fs::File::create(&tmp)?;
    let mut written: u64 = 0;
    for chunk_ref in &manifest.chunks {
        let ct = source.get(chunk_ref.index)?;
        let hash: [u8; 32] = Sha256::digest(&ct).into();
        if hash != chunk_ref.ciphertext_hash {
            return Err(AttachmentErrorKind::ChunkHashMismatch.into());
        }
        let km = chunk_key_material(&manifest.file_key, chunk_ref.index)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&km[..32]));
        let nonce = XNonce::from_slice(&km[32..56]);
        let plain = match cipher.decrypt(nonce, ct.as_ref()) {
            Ok(p) => p,
            Err(_) => {
                return Err(AttachmentErrorKind::AeadFailed.into());
            }
        };
        out.write_all(&plain)?;
        written = written.saturating_add(plain.len() as u64);
    }
    out.sync_all()?;
    if written != manifest.total_size {
        return Err(AttachmentErrorKind::SizeMismatch.into());
    }
    std::fs::rename(&tmp, output_path)?;
    // Renamed away: there is no temp left to clean up.
    cleanup.disarm();
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

    /// Two chunks: one full, one short. Two is the minimum that lets a
    /// failure happen *after* plaintext has already been written to the temp
    /// file — a single-chunk fixture would not exercise the leak at all.
    fn fixture_multi_chunk() -> (AttachmentManifest, Vec<Vec<u8>>, Vec<u8>) {
        let plaintext = vec![9u8; crate::attachment::CHUNK_SIZE + 7];
        let (manifest, chunks) =
            crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
        (manifest, chunks, plaintext)
    }

    /// A source that yields real chunks up to `fail_at`, then errors — to
    /// simulate an I/O failure part-way through reassembly.
    struct FailingSource {
        inner: MemSource,
        fail_at: u32,
    }
    impl ChunkSource for FailingSource {
        fn get(&self, index: u32) -> Result<Vec<u8>> {
            if index >= self.fail_at {
                // Any error works; this is the one `MemSource` itself returns
                // for a missing index, so the fixtures stay consistent.
                return Err(crate::attachment::AttachmentErrorKind::SizeMismatch.into());
            }
            self.inner.get(index)
        }
    }

    /// The `.part` path `reassemble` uses for a given output path.
    fn part_path(out: &std::path::Path) -> std::path::PathBuf {
        let mut s = out.as_os_str().to_owned();
        s.push(".part");
        std::path::PathBuf::from(s)
    }

    #[test]
    fn io_failure_midway_leaves_no_plaintext_behind() {
        // #156: the `.part` holds decrypted plaintext. An error part-way
        // through must not leave it on disk.
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, _plaintext) = fixture_multi_chunk();
        let source = FailingSource {
            inner: mem_source(chunks),
            fail_at: 1, // chunk 0 succeeds and is written, chunk 1 errors
        };

        let res = reassemble(&manifest, &source, &out);

        assert!(res.is_err(), "expected the source failure to propagate");
        assert!(!part_path(&out).exists(), "decrypted .part must be removed");
        assert!(!out.exists(), "no partial output");
    }

    #[test]
    fn panic_midway_leaves_no_plaintext_behind() {
        // The property explicit per-site cleanup cannot give us.
        struct PanickingSource(MemSource);
        impl ChunkSource for PanickingSource {
            fn get(&self, index: u32) -> Result<Vec<u8>> {
                if index >= 1 {
                    panic!("simulated panic mid-reassembly");
                }
                self.0.get(index)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, _plaintext) = fixture_multi_chunk();
        let source = PanickingSource(mem_source(chunks));

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reassemble(&manifest, &source, &out);
        }));

        assert!(res.is_err(), "the panic must propagate");
        assert!(
            !part_path(&out).exists(),
            "decrypted .part must be removed on unwind"
        );
    }

    #[test]
    fn success_leaves_no_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, plaintext) = fixture_multi_chunk();

        reassemble(&manifest, &mem_source(chunks), &out).unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), plaintext);
        assert!(!part_path(&out).exists(), "temp must not survive success");
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
