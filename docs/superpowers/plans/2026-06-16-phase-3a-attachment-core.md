# Phase 3.A — Attachment Core — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** turn a local file into an `AttachmentManifest` + encrypted chunk blobs, and turn received blobs back into a verified plaintext file — with image metadata stripped on send. Pure/local; no transport, no protocol frames.

**Architecture:** A new `crate::attachment` module: a CBOR `AttachmentManifest` (carried inside the existing `Kind::File`), a pure chunker (split → per-chunk HKDF-keyed XChaCha20-Poly1305 → SHA-256 ciphertext-hash), a pure reassembler (verify-hash-before-decrypt → AEAD → temp+rename), an image-metadata stripper, plus a `storage` transfer-state repo (migration 0015) and a ciphertext-only on-disk chunk store. No new crypto — existing `hkdf_expand` / `XChaCha20Poly1305` / `sha2`.

**Tech Stack:** Rust 2021, `chacha20poly1305` (XChaCha20-Poly1305, existing), `hkdf`+`sha2` (existing), `ciborium` (CBOR, existing), `rusqlite` (existing), `rand` (existing), `img-parts` (NEW — image metadata stripping).

**Spec:** `docs/superpowers/specs/2026-06-16-phase-3a-attachment-core-design.md`

---

## Conventions for every task

**Cargo isn't on PATH.** Prefix with `. "$HOME/.cargo/env" &&`.

**Per-task gates (run ALL before committing):**
```bash
. "$HOME/.cargo/env"
cargo fmt --all
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo test -p skattr-core --features test-harness
# Task 5 (new dependency) ALSO:
cargo deny check
```

**Hard rules:** GPLv3 header on every new `.rs`; no `unwrap`/`expect` in non-test code (`?` + typed `CoreError`); `todo!()` not `unimplemented!()`; never log file bytes / names / keys above `debug`; secrets (`file_key`, per-chunk key material) in `Zeroizing`; **no new crypto** (only existing primitives, domain-separated). Every gate green before commit. Commit trailer:
```
Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/core/src/attachment/mod.rs` | module root, re-exports, `CHUNK_SIZE`/`MAX_ATTACHMENT_BYTES` consts | 1 |
| `crates/core/src/attachment/error_kind.rs` | `AttachmentErrorKind` | 1 |
| `crates/core/src/attachment/manifest.rs` | `AttachmentManifest`/`ChunkRef` + CBOR encode/decode + version check + filename sanitize | 1 |
| `crates/core/src/error.rs` | `CoreError::Attachment(#[from] …)` | 1 |
| `crates/core/src/identity/derive.rs` | `INFO_ATTACH_V1` + `chunk_key_material` | 2 |
| `crates/core/src/attachment/chunker.rs` | pure `chunk_plaintext` | 3 |
| `crates/core/src/attachment/reassembler.rs` | `ChunkSource` trait + pure `reassemble` | 4 |
| `crates/core/src/attachment/strip.rs` | `strip_metadata` (img-parts) | 5 |
| `crates/core/src/storage/migrations/0015_attachments.sql` + `migrations.rs` | `attachments` + `attachment_chunks` tables | 6 |
| `crates/core/src/storage/attachments.rs` + `storage/mod.rs` | `AttachmentRepo` | 6 |
| `crates/core/src/attachment/store.rs` | on-disk ciphertext chunk store (`ChunkSource` impl) | 6 |
| `crates/core/src/lib.rs` | `pub(crate) mod attachment;` | 1 |

**Task order:** 1 (manifest+errors) → 2 (keying) → 3 (chunker) → 4 (reassembler) → 5 (strip) → 6 (storage+store) → 7 (end-to-end + gate). Tasks 1–5 are pure (no disk); 6 adds persistence; 7 composes.

---

## Task 1: Manifest type, AttachmentErrorKind, CoreError wiring, module scaffold

**Files:**
- Create: `crates/core/src/attachment/mod.rs`, `crates/core/src/attachment/error_kind.rs`, `crates/core/src/attachment/manifest.rs`
- Modify: `crates/core/src/lib.rs`, `crates/core/src/error.rs`

- [ ] **Step 1: Scaffold the module.** Create `crates/core/src/attachment/mod.rs`:
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Attachment core (Phase 3.A): manifest format, chunker, reassembler,
//! metadata stripping. Pure/local — no transport. The manifest rides inside
//! MLS via `envelope::kinds::Kind::File`.

pub(crate) mod error_kind;
pub(crate) mod manifest;

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
```
Add `pub(crate) mod attachment;` to `crates/core/src/lib.rs` (alongside `pub(crate) mod delivery;` etc., alphabetical).

- [ ] **Step 2: Add `AttachmentErrorKind`** in `attachment/error_kind.rs`:
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed attachment failures (payload of `CoreError::Attachment`).

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AttachmentErrorKind {
    /// Total plaintext size exceeds `MAX_ATTACHMENT_BYTES`.
    #[error("attachment too large")]
    TooLarge,
    /// A chunk ciphertext's SHA-256 did not match the manifest `ChunkRef`.
    #[error("attachment chunk hash mismatch")]
    ChunkHashMismatch,
    /// XChaCha20-Poly1305 decryption (tag verification) failed.
    #[error("attachment chunk decryption failed")]
    AeadFailed,
    /// Reassembled size or chunk count disagrees with the manifest.
    #[error("attachment size/shape mismatch")]
    SizeMismatch,
    /// Manifest is malformed or has an unknown version.
    #[error("attachment manifest invalid: {0}")]
    ManifestInvalid(String),
    /// Metadata could not be stripped from a supported type.
    #[error("attachment metadata strip failed: {0}")]
    StripFailed(String),
}
```

- [ ] **Step 3: Wire `CoreError`.** In `crates/core/src/error.rs`, add a variant to `CoreError` (after `Delivery`, matching the `#[from]` sub-enum pattern):
```rust
    /// Attachment (Phase 3) chunking / manifest / metadata problem.
    #[error("{0}")]
    Attachment(#[from] crate::attachment::AttachmentErrorKind),
```
No `kind()` arm is needed — the `_ => None` catch-all + `#[non_exhaustive]` cover it, and the `kind_has_no_string_matching` guard test still passes (no `.contains` added). Verify the crate still compiles.

- [ ] **Step 4: Write the failing manifest test** in `attachment/manifest.rs` `#[cfg(test)] mod tests`:
```rust
#[test]
fn manifest_round_trips_cbor() {
    let m = AttachmentManifest {
        manifest_version: crate::attachment::MANIFEST_VERSION,
        attachment_id: [0x11; 16],
        filename: "photo.jpg".into(),
        mime: "image/jpeg".into(),
        total_size: 1000,
        chunk_size: 262_144,
        file_key: [0x22; 32],
        chunks: vec![ChunkRef { index: 0, ciphertext_hash: [0x33; 32], len: 1016 }],
    };
    let bytes = m.to_cbor().unwrap();
    let back = AttachmentManifest::from_cbor(&bytes).unwrap();
    assert_eq!(back.attachment_id, m.attachment_id);
    assert_eq!(back.chunks.len(), 1);
    assert_eq!(back.chunks[0].ciphertext_hash, [0x33; 32]);
}

#[test]
fn manifest_rejects_unknown_version() {
    let mut m = sample_manifest();
    m.manifest_version = 99;
    let bytes = m.to_cbor().unwrap();
    let err = AttachmentManifest::from_cbor(&bytes).expect_err("must reject");
    assert!(matches!(err, CoreError::Attachment(AttachmentErrorKind::ManifestInvalid(_))));
}

#[test]
fn sanitize_filename_strips_path_and_control() {
    assert_eq!(manifest::sanitize_filename("../../etc/passwd"), "passwd");
    assert_eq!(manifest::sanitize_filename("a/b\\c.txt"), "c.txt");
    assert_eq!(manifest::sanitize_filename(""), "attachment");
}
```
(`sample_manifest()` is a small test helper building a valid manifest.)

- [ ] **Step 5: Run, verify it fails to compile** (types/methods absent):
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness manifest_round_trips_cbor
```

- [ ] **Step 6: Implement `manifest.rs`:**
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Attachment manifest: the CBOR descriptor carried inside `Kind::File`.

use serde::{Deserialize, Serialize};

use crate::attachment::error_kind::AttachmentErrorKind;
use crate::attachment::MANIFEST_VERSION;
use crate::error::{CoreError, Result};

/// Per-chunk descriptor: content address + length of the chunk ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: u32,
    pub ciphertext_hash: [u8; 32],
    pub len: u32,
}

/// Attachment manifest. Confidential + authenticated in transit because it
/// travels as an MLS app message (`Kind::File`). Holds the file key from which
/// per-chunk keys are derived (HKDF), so chunk blobs are opaque ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentManifest {
    pub manifest_version: u8,
    pub attachment_id: [u8; 16],
    pub filename: String,
    pub mime: String,
    pub total_size: u64,
    pub chunk_size: u32,
    pub file_key: [u8; 32],
    pub chunks: Vec<ChunkRef>,
}

impl AttachmentManifest {
    /// CBOR-encode for embedding in `Kind::File { manifest }`.
    pub fn to_cbor(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf)
            .map_err(|e| CoreError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Decode + validate a manifest. Rejects an unknown `manifest_version`.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self> {
        let m: AttachmentManifest = ciborium::from_reader(bytes)
            .map_err(|e| CoreError::CborDecode(e.to_string()))?;
        if m.manifest_version != MANIFEST_VERSION {
            return Err(AttachmentErrorKind::ManifestInvalid(format!(
                "unknown manifest version {}",
                m.manifest_version
            ))
            .into());
        }
        Ok(m)
    }
}

/// Reduce a sender-supplied filename to a safe basename: drop any path
/// components and control characters; empty → "attachment".
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim_matches('.');
    let cleaned: String = base.chars().filter(|c| !c.is_control() && *c != '/' && *c != '\\').collect();
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}
```
> NOTE: `[u8; 32]`/`[u8; 16]` serde-derive fine (≤ 32). `[u8; 32]` for `file_key` + `ciphertext_hash` are exactly 32 — no `BigArray`. `file_key` in the struct is sensitive; it is created inside `Zeroizing` in the chunker (Task 3) and only moved into the manifest at the end — note this is acceptable since the manifest's destiny is MLS encryption.

- [ ] **Step 7: Run the tests, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness manifest::
```

- [ ] **Step 8: Per-task gates + commit:**
```bash
git add crates/core/src/attachment/ crates/core/src/lib.rs crates/core/src/error.rs
git commit -m "feat(3.A): AttachmentManifest + AttachmentErrorKind + module scaffold

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Per-chunk key derivation

**Files:**
- Modify: `crates/core/src/identity/derive.rs`
- Test: `derive.rs` `mod tests`

- [ ] **Step 1: Write the failing test** in `derive.rs` `mod tests`:
```rust
#[test]
fn chunk_key_material_is_deterministic_and_per_index() {
    let fk = [0x42u8; 32];
    let a = chunk_key_material(&fk, 0).unwrap();
    let b = chunk_key_material(&fk, 0).unwrap();
    assert_eq!(*a, *b, "same (file_key, index) → same material");
    let c = chunk_key_material(&fk, 1).unwrap();
    assert_ne!(*a, *c, "different index → different material");
    // key (0..32) and nonce (32..56) within one chunk must not collide
    assert_ne!(&a[..32], &a[24..56]);
}
```

- [ ] **Step 2: Run, verify it fails** (fn absent):
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness chunk_key_material_is_deterministic
```

- [ ] **Step 3: Implement.** Add the label + helper to `derive.rs`:
```rust
/// Attachment per-chunk key derivation:
/// `HKDF(file_key, "skattr-attach-v1" || u32_be(chunk_index))` → 56 bytes
/// (32-byte XChaCha20-Poly1305 key ‖ 24-byte XNonce).
pub const INFO_ATTACH_V1: &[u8] = b"skattr-attach-v1";

/// Derive a chunk's 32-byte AEAD key + 24-byte nonce from the manifest
/// `file_key` and the chunk index. Returns 56 bytes: `[0..32]` key,
/// `[32..56]` nonce.
pub fn chunk_key_material(file_key: &[u8; 32], index: u32) -> Result<Zeroizing<[u8; 56]>> {
    let mut info = Vec::with_capacity(INFO_ATTACH_V1.len() + 4);
    info.extend_from_slice(INFO_ATTACH_V1);
    info.extend_from_slice(&index.to_be_bytes());
    hkdf_expand::<56>(file_key, &info)
}
```

- [ ] **Step 4: Run the test, verify PASS:**
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness chunk_key_material
```

- [ ] **Step 5: Per-task gates + commit:**
```bash
git add crates/core/src/identity/derive.rs
git commit -m "feat(3.A): per-chunk HKDF key derivation (skattr-attach-v1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Chunker (pure)

**Files:**
- Create: `crates/core/src/attachment/chunker.rs`
- Modify: `crates/core/src/attachment/mod.rs` (`pub(crate) mod chunker;`)
- Test: `chunker.rs` `mod tests`

- [ ] **Step 1: Write the failing test:**
```rust
#[test]
fn chunks_plaintext_into_manifest_and_ciphertext() {
    let plaintext = vec![7u8; 262_144 * 2 + 100]; // 2 full chunks + a partial
    let (manifest, chunks) = chunk_plaintext(&plaintext, "f.bin", "application/octet-stream").unwrap();
    assert_eq!(manifest.manifest_version, crate::attachment::MANIFEST_VERSION);
    assert_eq!(manifest.total_size, plaintext.len() as u64);
    assert_eq!(manifest.chunk_size, crate::attachment::CHUNK_SIZE as u32);
    assert_eq!(manifest.chunks.len(), 3);
    assert_eq!(chunks.len(), 3);
    // each ChunkRef hash matches the produced ciphertext
    for (i, ct) in chunks.iter().enumerate() {
        let h: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(ct).into();
        assert_eq!(manifest.chunks[i].ciphertext_hash, h);
        assert_eq!(manifest.chunks[i].len as usize, ct.len());
    }
}

#[test]
fn chunker_rejects_oversize() {
    // Don't allocate 100 MiB: chunk a small buffer with a temporarily-tiny cap
    // is not possible (const), so assert on a >cap length via a guard helper.
    // Instead test the boundary using a wrapper that checks the cap path.
    let err = chunk_plaintext_with_cap(&[0u8; 8], "x", "y", 4).expect_err("must reject");
    assert!(matches!(err, CoreError::Attachment(AttachmentErrorKind::TooLarge)));
}
```
> NOTE: to test the oversize path without allocating 100 MiB, factor the cap into a `pub(crate) fn chunk_plaintext_with_cap(plaintext, filename, mime, max_bytes)` that `chunk_plaintext` calls with `MAX_ATTACHMENT_BYTES`. The test drives the cap path with a tiny cap.

- [ ] **Step 2: Run, verify it fails** (fn absent).

- [ ] **Step 3: Implement `chunker.rs`:**
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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
        let ct = cipher
            .encrypt(nonce, plain)
            .map_err(|_| AttachmentErrorKind::AeadFailed)?;
        let hash: [u8; 32] = Sha256::digest(&ct).into();
        let len = u32::try_from(ct.len()).map_err(|_| AttachmentErrorKind::SizeMismatch)?;
        refs.push(ChunkRef { index, ciphertext_hash: hash, len });
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
```
Add `pub(crate) mod chunker;` to `attachment/mod.rs`.

- [ ] **Step 4: Run the tests, verify PASS.** **Step 5:** per-task gates + commit:
```bash
git add crates/core/src/attachment/chunker.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.A): pure chunker (split + per-chunk AEAD + SHA-256 hash)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Reassembler (pure) + ChunkSource

**Files:**
- Create: `crates/core/src/attachment/reassembler.rs`
- Modify: `crates/core/src/attachment/mod.rs` (`pub(crate) mod reassembler;`)
- Test: `reassembler.rs` `mod tests`

- [ ] **Step 1: Write the failing test:**
```rust
struct MemSource(std::collections::HashMap<u32, Vec<u8>>);
impl ChunkSource for MemSource {
    fn get(&self, index: u32) -> Result<Vec<u8>> {
        self.0.get(&index).cloned().ok_or_else(|| {
            crate::attachment::AttachmentErrorKind::SizeMismatch.into()
        })
    }
}

#[test]
fn round_trips_byte_identical() {
    let plaintext = vec![9u8; 262_144 + 7];
    let (manifest, chunks) = crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
    let src = MemSource(chunks.into_iter().enumerate().map(|(i, c)| (i as u32, c)).collect());
    let out = tempfile::NamedTempFile::new().unwrap();
    reassemble(&manifest, &src, out.path()).unwrap();
    assert_eq!(std::fs::read(out.path()).unwrap(), plaintext);
}

#[test]
fn flipped_ciphertext_byte_fails_hash_check() {
    let plaintext = vec![1u8; 100];
    let (manifest, mut chunks) = crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
    chunks[0][0] ^= 0xFF; // corrupt before the hash
    let src = MemSource(chunks.into_iter().enumerate().map(|(i, c)| (i as u32, c)).collect());
    let out = tempfile::NamedTempFile::new().unwrap();
    let err = reassemble(&manifest, &src, out.path()).expect_err("must reject");
    assert!(matches!(err, CoreError::Attachment(AttachmentErrorKind::ChunkHashMismatch)));
}

#[test]
fn flipped_tag_fails_aead() {
    let plaintext = vec![1u8; 100];
    let (mut manifest, mut chunks) = crate::attachment::chunker::chunk_plaintext(&plaintext, "f", "m").unwrap();
    // Flip the last ciphertext byte (AEAD tag) AND update the manifest hash so
    // it passes the hash gate and fails at decrypt instead.
    let last = chunks[0].len() - 1;
    chunks[0][last] ^= 0xFF;
    manifest.chunks[0].ciphertext_hash =
        <sha2::Sha256 as sha2::Digest>::digest(&chunks[0]).into();
    manifest.chunks[0].len = chunks[0].len() as u32;
    let src = MemSource(chunks.into_iter().enumerate().map(|(i, c)| (i as u32, c)).collect());
    let out = tempfile::NamedTempFile::new().unwrap();
    let err = reassemble(&manifest, &src, out.path()).expect_err("must reject");
    assert!(matches!(err, CoreError::Attachment(AttachmentErrorKind::AeadFailed)));
}
```

- [ ] **Step 2: Run, verify it fails** (trait/fn absent).

- [ ] **Step 3: Implement `reassembler.rs`:**
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Pure reassembler: manifest + chunk source → verified plaintext file.

use std::io::Write;
use std::path::Path;

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use crate::attachment::error_kind::AttachmentErrorKind;
use crate::attachment::manifest::AttachmentManifest;
use crate::error::{CoreError, Result};
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
    let tmp = output_path.with_extension("part");
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
        written += plain.len() as u64;
    }
    out.sync_all()?;
    if written != manifest.total_size {
        let _ = std::fs::remove_file(&tmp);
        return Err(AttachmentErrorKind::SizeMismatch.into());
    }
    std::fs::rename(&tmp, output_path).map_err(CoreError::Io)?;
    Ok(())
}
```
Add `pub(crate) mod reassembler;` to `attachment/mod.rs`.

- [ ] **Step 4: Run the tests, verify PASS.** **Step 5:** per-task gates + commit:
```bash
git add crates/core/src/attachment/reassembler.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.A): pure reassembler (verify-hash-before-decrypt, temp+rename)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Metadata stripping (img-parts)

**Files:**
- Modify: `crates/core/Cargo.toml` (add `img-parts`)
- Create: `crates/core/src/attachment/strip.rs`
- Modify: `crates/core/src/attachment/mod.rs` (`pub(crate) mod strip;`)
- Test: `strip.rs` `mod tests`

- [ ] **Step 1: Add the dependency.** In `crates/core/Cargo.toml` add `img-parts = "0.3"` (confirm the latest 0.3.x). Run `cargo deny check` — if `img-parts` or a transitive dep trips the license allowlist / advisories, STOP and report (do not bypass `cargo-deny`); we'd pick an alternative (`little_exif`) or narrow scope.

- [ ] **Step 2: Write the failing test** in `strip.rs` `mod tests`. Use a tiny JPEG fixture with a planted EXIF/APP1 segment (construct one in-test via `img_parts`, or check in a small `tests/fixtures/exif.jpg`):
```rust
#[test]
fn strips_exif_from_jpeg_keeps_pixels() {
    // `with_exif` builds a minimal JPEG carrying an APP1/EXIF segment.
    let input = test_jpeg_with_exif();
    assert!(has_exif(&input), "fixture must start with EXIF");
    let (stripped, mime) = strip_metadata(&input, "image/jpeg").unwrap();
    assert_eq!(mime, "image/jpeg");
    assert!(!has_exif(&stripped), "EXIF must be gone after strip");
    // pixel/scan data preserved: re-decodes to the same dimensions
    assert_eq!(jpeg_dimensions(&stripped), jpeg_dimensions(&input));
}

#[test]
fn passes_through_non_image() {
    let bytes = b"not an image".to_vec();
    let (out, mime) = strip_metadata(&bytes, "application/octet-stream").unwrap();
    assert_eq!(out, bytes);
    assert_eq!(mime, "application/octet-stream");
}
```
> NOTE: implement `test_jpeg_with_exif`/`has_exif`/`jpeg_dimensions` test helpers using `img_parts` (and `image` only if already available — otherwise check JPEG SOF markers manually). If constructing an EXIF JPEG in-test is awkward with `img-parts`, check in a ≤1 KB fixture under `crates/core/tests/fixtures/` and `include_bytes!` it.

- [ ] **Step 3: Run, verify it fails** (fn absent).

- [ ] **Step 4: Implement `strip.rs`** using `img-parts` to drop metadata segments without re-encoding pixels (JPEG: remove APP1/EXIF + other APPn metadata; PNG: remove ancillary text/`eXIf` chunks). Representative shape (confirm exact `img-parts` 0.3 API):
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Send-side metadata stripping for common image formats.

use crate::attachment::error_kind::AttachmentErrorKind;
use crate::error::Result;

/// Strip EXIF/metadata from supported image types; pass others through.
/// Returns `(bytes, effective_mime)`.
pub(crate) fn strip_metadata(bytes: &[u8], mime: &str) -> Result<(Vec<u8>, String)> {
    match detect_kind(bytes, mime) {
        ImageKind::Jpeg => strip_jpeg(bytes).map(|b| (b, "image/jpeg".to_string())),
        ImageKind::Png => strip_png(bytes).map(|b| (b, "image/png".to_string())),
        ImageKind::Other => Ok((bytes.to_vec(), mime.to_string())),
    }
}
// detect_kind: sniff magic bytes (JPEG FF D8 FF, PNG 89 50 4E 47), fall back to mime.
// strip_jpeg / strip_png: use img_parts to drop metadata segments; map any
// parse error to AttachmentErrorKind::StripFailed (do NOT silently pass a
// malformed image through — a sender claiming image/jpeg with junk is rejected).
```
> IMPLEMENTER: confirm the `img-parts` 0.3 API for reading a `Jpeg`/`Png`, removing EXIF / metadata segments, and re-emitting bytes. Keep `strip_jpeg`/`strip_png` small and total; no `unwrap`/`expect`.

- [ ] **Step 5: Run the tests, verify PASS.**

- [ ] **Step 6: Per-task gates (incl. `cargo deny check`) + commit:**
```bash
git add crates/core/Cargo.toml crates/core/src/attachment/strip.rs crates/core/src/attachment/mod.rs
# + any checked-in fixture
git commit -m "feat(3.A): image EXIF/metadata stripping via img-parts

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Storage — migration 0015 + AttachmentRepo + on-disk chunk store

**Files:**
- Create: `crates/core/src/storage/migrations/0015_attachments.sql`
- Modify: `crates/core/src/storage/migrations.rs` (append to `ALL_MIGRATIONS`)
- Create: `crates/core/src/storage/attachments.rs` (`AttachmentRepo`)
- Modify: `crates/core/src/storage/mod.rs` (`pub(crate) mod attachments;` + `pub use`)
- Create: `crates/core/src/attachment/store.rs` (on-disk chunk store + `ChunkSource` impl)
- Modify: `crates/core/src/attachment/mod.rs` (`pub(crate) mod store;`)
- Test: `attachments.rs` + `store.rs` `mod tests`

- [ ] **Step 1: Write the migration** `0015_attachments.sql`:
```sql
-- Phase 3.A: attachment transfer state.
CREATE TABLE IF NOT EXISTS attachments (
    attachment_id BLOB PRIMARY KEY,            -- 16 random bytes
    direction     TEXT NOT NULL CHECK (direction IN ('out', 'in')),
    manifest      BLOB NOT NULL,               -- CBOR AttachmentManifest
    total_chunks  INTEGER NOT NULL,
    status        TEXT NOT NULL,               -- 'pending' | 'complete' | 'failed'
    created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS attachment_chunks (
    attachment_id BLOB NOT NULL,
    chunk_index   INTEGER NOT NULL,
    received      INTEGER NOT NULL DEFAULT 0,  -- 0/1
    PRIMARY KEY (attachment_id, chunk_index)
);
```
Append to `ALL_MIGRATIONS` in `migrations.rs`:
```rust
    Migration {
        version: 15,
        sql: include_str!("migrations/0015_attachments.sql"),
    },
```
(Match the existing `Migration { version, sql }` shape + field names exactly.)

- [ ] **Step 2: Write the failing repo test** in `storage/attachments.rs` `mod tests` (model on `read_state.rs` — uses `Pool::in_memory()`):
```rust
#[test]
fn insert_and_get_and_mark_received_round_trip() {
    let pool = Pool::in_memory();
    let repo = AttachmentRepo::new(&pool);
    repo.insert(&[0x11; 16], "out", b"manifest-bytes", 3, 100).unwrap();
    let row = repo.get(&[0x11; 16]).unwrap().unwrap();
    assert_eq!(row.total_chunks, 3);
    assert_eq!(row.status, "pending");
    assert_eq!(repo.received_indices(&[0x11; 16]).unwrap(), Vec::<u32>::new());
    repo.mark_received(&[0x11; 16], 0).unwrap();
    repo.mark_received(&[0x11; 16], 2).unwrap();
    assert_eq!(repo.received_indices(&[0x11; 16]).unwrap(), vec![0, 2]);
    repo.set_status(&[0x11; 16], "complete").unwrap();
    assert_eq!(repo.get(&[0x11; 16]).unwrap().unwrap().status, "complete");
}
```

- [ ] **Step 3: Run, verify it fails.**

- [ ] **Step 4: Implement `AttachmentRepo`** in `storage/attachments.rs` (same `pool.with`/`with_mut` + `StorageErrorKind::Other(format!(...))` pattern as `read_state.rs`). Define a small `AttachmentRow { direction: String, manifest: Vec<u8>, total_chunks: i64, status: String, created_at: i64 }`. Methods: `new`, `insert(attachment_id, direction, manifest, total_chunks, created_at)`, `get(attachment_id) -> Option<AttachmentRow>`, `mark_received(attachment_id, index)` (INSERT OR REPLACE into `attachment_chunks` with received=1), `received_indices(attachment_id) -> Vec<u32>` (ordered), `set_status(attachment_id, status)`, `delete(attachment_id)`. Export via `pub use attachments::AttachmentRepo;` + `pub(crate) mod attachments;` in `storage/mod.rs`.

- [ ] **Step 5: Implement the on-disk chunk store** in `attachment/store.rs`:
```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! On-disk ciphertext chunk store at `<data_dir>/attachments/<hex id>/<index>`.
//! Blobs are AEAD ciphertext (keys live in the MLS-protected manifest), so this
//! is not an at-rest plaintext gap.

use std::path::{Path, PathBuf};

use crate::attachment::reassembler::ChunkSource;
use crate::error::{CoreError, Result};

pub(crate) struct ChunkStore {
    root: PathBuf, // <data_dir>/attachments
}

impl ChunkStore {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self { root: data_dir.join("attachments") }
    }
    fn dir(&self, attachment_id: &[u8; 16]) -> PathBuf {
        self.root.join(hex::encode(attachment_id))
    }
    pub(crate) fn put(&self, attachment_id: &[u8; 16], index: u32, ciphertext: &[u8]) -> Result<()> {
        let d = self.dir(attachment_id);
        std::fs::create_dir_all(&d)?;
        let tmp = d.join(format!("{index}.part"));
        std::fs::write(&tmp, ciphertext)?;
        std::fs::rename(&tmp, d.join(index.to_string())).map_err(CoreError::Io)?;
        Ok(())
    }
    pub(crate) fn get_chunk(&self, attachment_id: &[u8; 16], index: u32) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.dir(attachment_id).join(index.to_string()))?)
    }
    pub(crate) fn remove(&self, attachment_id: &[u8; 16]) -> Result<()> {
        let d = self.dir(attachment_id);
        if d.exists() { std::fs::remove_dir_all(&d)?; }
        Ok(())
    }
}

/// Adapter so a `ChunkStore` (bound to one attachment) is a `ChunkSource`.
pub(crate) struct StoreSource<'s> {
    pub(crate) store: &'s ChunkStore,
    pub(crate) attachment_id: [u8; 16],
}
impl ChunkSource for StoreSource<'_> {
    fn get(&self, index: u32) -> Result<Vec<u8>> {
        self.store.get_chunk(&self.attachment_id, index)
    }
}
```
(`hex` is an existing dep — used in `pool.rs`/`daemon::hex`.) Add `pub(crate) mod store;` to `attachment/mod.rs`. Add a `store.rs` test: put → get_chunk round-trips; remove deletes the dir; `StoreSource` reads back via `ChunkSource`.

- [ ] **Step 6: Run the tests, verify PASS** (`attachments::` + `store::`; the migrations `schema_version` count test auto-covers 0015).

- [ ] **Step 7: Per-task gates + commit:**
```bash
git add crates/core/src/storage/migrations/0015_attachments.sql crates/core/src/storage/migrations.rs crates/core/src/storage/attachments.rs crates/core/src/storage/mod.rs crates/core/src/attachment/store.rs crates/core/src/attachment/mod.rs
git commit -m "feat(3.A): attachment storage — migration 0015 + AttachmentRepo + on-disk chunk store

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: End-to-end local pipeline + final verification

**Files:**
- Modify: `crates/core/src/attachment/mod.rs` (a `prepare` + `finish` convenience pair, or a `mod tests` end-to-end test)
- Test: `attachment/mod.rs` `mod tests`

- [ ] **Step 1: Write the end-to-end test** in `attachment/mod.rs` `mod tests`, composing strip → chunk → store → reassemble-from-store:
```rust
#[test]
fn end_to_end_local_round_trip_through_store() {
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::attachment::store::ChunkStore::new(tmp.path());

    let original = vec![0xABu8; crate::attachment::CHUNK_SIZE + 12345];
    // (non-image bytes pass strip through unchanged)
    let (stripped, mime) = crate::attachment::strip::strip_metadata(&original, "application/octet-stream").unwrap();
    let (manifest, chunks) = crate::attachment::chunker::chunk_plaintext(&stripped, "blob.bin", &mime).unwrap();

    // stage ciphertext chunks
    for (i, ct) in chunks.iter().enumerate() {
        store.put(&manifest.attachment_id, i as u32, ct).unwrap();
    }
    // reassemble from the store
    let out = tmp.path().join("out.bin");
    let src = crate::attachment::store::StoreSource { store: &store, attachment_id: manifest.attachment_id };
    crate::attachment::reassembler::reassemble(&manifest, &src, &out).unwrap();

    assert_eq!(std::fs::read(&out).unwrap(), original);
    // manifest survives a CBOR round-trip (as it would inside Kind::File)
    let back = crate::attachment::AttachmentManifest::from_cbor(&manifest.to_cbor().unwrap()).unwrap();
    assert_eq!(back.attachment_id, manifest.attachment_id);
}
```

- [ ] **Step 2: Run it, verify PASS** (all units already exist from Tasks 1–6; this is the integration assertion):
```bash
. "$HOME/.cargo/env" && cargo test -p skattr-core --features test-harness end_to_end_local_round_trip
```

- [ ] **Step 3: FULL final gate** (CI-parity):
```bash
. "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --all-features -- -D warnings
cargo deny check
cargo test -p skattr-core --features test-harness
cargo test -p skattr-tests -- --test-threads=1
cargo build -p skattr-cli
```
Expected: fmt clean; clippy clean; `cargo deny` clean (the new `img-parts` dep + transitives pass the allowlist/advisories); core green (incl. all new `attachment::` + `storage::attachments` + `storage::store` tests); `skattr-tests` all non-ignored green (regression — 3.A adds no transport, so the existing guardrails are unaffected); CLI builds.

- [ ] **Step 4: Commit:**
```bash
git add crates/core/src/attachment/mod.rs
git commit -m "test(3.A): end-to-end local attachment round-trip (strip→chunk→store→reassemble)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review (against the spec)

**Spec coverage:**
- A1 manifest (in `Kind::File`, version-checked, sanitized filename) → Task 1 ✓
- A2 chunker (HKDF-per-chunk keying via decision 1) → Tasks 2 + 3 ✓
- A3 reassembler (verify-hash-before-decrypt, temp+rename) → Task 4 ✓
- A4 metadata strip (image EXIF, img-parts) → Task 5 ✓
- A5 chunk store + transfer-state repo (migration 0015) → Task 6 ✓
- A6 `AttachmentErrorKind` + caps → Task 1 ✓
- Exit criteria 1 (byte-identical round-trip) → Tasks 4 + 7; 2 (tamper/tag rejected) → Task 4; 3 (oversize) → Task 3; 4 (EXIF stripped / passthrough) → Task 5; 5 (manifest CBOR + unknown version) → Task 1; 6 (store + repo round-trip) → Task 6; 7 (gates + cargo-deny) → Task 7 ✓

**Type/signature consistency:** `AttachmentManifest`/`ChunkRef` field names + `to_cbor`/`from_cbor` (Task 1) are used verbatim in Tasks 3/4/7. `chunk_key_material(&[u8;32], u32) -> Zeroizing<[u8;56]>` (Task 2) is called identically in the chunker (3) and reassembler (4). `ChunkSource::get(u32) -> Result<Vec<u8>>` (Task 4) is implemented by `StoreSource` (Task 6) and the test `MemSource` (Task 4). `CHUNK_SIZE`/`MAX_ATTACHMENT_BYTES`/`MANIFEST_VERSION` (Task 1) are referenced consistently. `AttachmentErrorKind` variants are used exactly as defined. `Migration { version, sql }` matches `migrations.rs`.

**Placeholder scan:** no TBD/TODO; every code step shows real code. The `img-parts` API details (Task 5) and the JPEG-EXIF test fixture carry explicit IMPLEMENTER notes (confirm the 0.3 API / check in a fixture) — deliberate, because the exact crate API must be verified against the resolved version, and `cargo deny` must pass before the crate is locked in. If `img-parts` fails `cargo-deny`, the implementer stops and reports (a known decision point, not a silent guess).

**Security invariants:** no new crypto (existing XChaCha20-Poly1305 + SHA-256 + HKDF, domain-separated `"skattr-attach-v1"`); `file_key` + per-chunk material in `Zeroizing`; staged blobs are ciphertext (no plaintext at rest); hash-check before decrypt; filename sanitized (no path traversal); reassembly is temp+rename (no partial output); no transport/wire/protocol change in 3.A.
