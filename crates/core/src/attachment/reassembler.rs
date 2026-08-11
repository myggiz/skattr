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
    // Append rather than `with_extension` (which would *replace* the output's
    // extension, so `a.pdf` and `a.txt` would collide on `a.part`).
    //
    // The suffix is unique per invocation because two concurrent reassemblies
    // can target the same output: `open_attachment_cmd` derives its path
    // deterministically from the attachment id, so a double-click yields two
    // `spawn_blocking` runs writing the same file. With one shared `.part`
    // they interleave writes and each one's cleanup unlinks the other's live
    // temp. Uniqueness makes each run's temp its own to write and delete.
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp_os = output_path.as_os_str().to_owned();
    tmp_os.push(format!(".part-{}-{seq}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_os);
    // The temp file holds DECRYPTED plaintext of an attachment that is
    // otherwise kept encrypted at rest, so no error path may leave it behind
    // — not a validation error, not a disk error (#156). It also covers a
    // panic, but only in unwinding builds: release builds set `panic =
    // "abort"` (Cargo.toml), and `Drop` does not run on abort. An earlier
    // version cleaned up only the validation paths on the grounds that a
    // stray `.part` is never mistaken for real output; true, but it answers
    // a correctness question rather than the security one.
    //
    // A failed removal (read-only mount, changed directory permissions) cannot
    // be propagated out of `Drop`, and the caller sees only the original
    // error — so warn, or the surviving plaintext leaves no trace at all.
    // Logged without the path: it carries the attachment's filename.
    let cleanup = crate::on_drop::OnDrop::new({
        let tmp = tmp.clone();
        move || match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(error = %e, "reassembly temp cleanup failed; decrypted plaintext may remain")
            }
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

    /// Every temp file `reassemble` could have left beside `out`.
    ///
    /// The temp name carries a unique per-invocation suffix, so these tests
    /// cannot reconstruct it — and asserting a *guessed* path does not exist
    /// would pass whether or not cleanup works. Scan the directory instead:
    /// the property under test is "no plaintext survives", not "one specific
    /// filename is absent".
    fn leftover_temps(out: &std::path::Path) -> Vec<std::path::PathBuf> {
        let (Some(dir), Some(name)) = (out.parent(), out.file_name()) else {
            return Vec::new();
        };
        let prefix = format!("{}.part", name.to_string_lossy());
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
            })
            .collect()
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
        assert!(
            leftover_temps(&out).is_empty(),
            "decrypted temp must be removed"
        );
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
            leftover_temps(&out).is_empty(),
            "decrypted temp must be removed on unwind"
        );
    }

    #[test]
    fn rename_failure_after_full_write_leaves_no_plaintext_behind() {
        // #156's literal scenario: everything up to and including `sync_all`
        // succeeds — the full plaintext is written to `.part` — and only the
        // final `rename` fails (disk-full / read-only-mount territory).
        // Forced here by making `output_path` an existing directory: the
        // write completes normally, then `std::fs::rename` fails because you
        // cannot rename a file onto a directory.
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        std::fs::create_dir(&out).unwrap();
        let (manifest, chunks, _plaintext) = fixture_multi_chunk();

        let res = reassemble(&manifest, &mem_source(chunks), &out);

        assert!(res.is_err(), "expected the rename failure to propagate");
        assert!(
            leftover_temps(&out).is_empty(),
            "decrypted temp must be removed after a post-write rename failure"
        );
    }

    #[test]
    fn a_concurrent_failed_reassembly_does_not_disturb_a_live_one() {
        // Two reassemblies can target the same output at once —
        // `open_attachment_cmd` derives its path from the attachment id, so a
        // double-click runs it twice. When both used the same temp name, the
        // failing run's cleanup unlinked the live run's temp and the live run
        // then failed its rename, so one user action broke another.
        //
        // Sequenced with channels rather than timing: A writes chunk 0 and
        // parks, B runs to completion and fails, then A is released to finish.
        use std::sync::mpsc;

        struct GatedSource {
            inner: MemSource,
            a_wrote_chunk0: mpsc::Sender<()>,
            b_finished: std::sync::Mutex<mpsc::Receiver<()>>,
        }
        impl ChunkSource for GatedSource {
            fn get(&self, index: u32) -> Result<Vec<u8>> {
                if index == 1 {
                    // Chunk 0 is already written to A's temp file.
                    let _ = self.a_wrote_chunk0.send(());
                    if let Ok(rx) = self.b_finished.lock() {
                        let _ = rx.recv();
                    }
                }
                self.inner.get(index)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared.bin");
        let (manifest, chunks, plaintext) = fixture_multi_chunk();
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();

        let gated = GatedSource {
            inner: mem_source(chunks.clone()),
            a_wrote_chunk0: tx_a,
            b_finished: std::sync::Mutex::new(rx_b),
        };
        let (m_a, o_a) = (manifest.clone(), out.clone());
        let a = std::thread::spawn(move || reassemble(&m_a, &gated, &o_a));

        rx_a.recv().expect("A must reach chunk 1");
        // B fails immediately, so its cleanup runs while A's temp is live.
        let failing = FailingSource {
            inner: mem_source(chunks),
            fail_at: 0,
        };
        assert!(
            reassemble(&manifest, &failing, &out).is_err(),
            "B is expected to fail"
        );
        tx_b.send(()).unwrap();

        assert!(
            a.join().unwrap().is_ok(),
            "B's cleanup must not break A's reassembly"
        );
        assert_eq!(std::fs::read(&out).unwrap(), plaintext);
        assert!(leftover_temps(&out).is_empty(), "no temp may survive");
    }

    #[test]
    fn success_leaves_no_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bin");
        let (manifest, chunks, plaintext) = fixture_multi_chunk();

        reassemble(&manifest, &mem_source(chunks), &out).unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), plaintext);
        assert!(
            leftover_temps(&out).is_empty(),
            "temp must not survive success"
        );
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
