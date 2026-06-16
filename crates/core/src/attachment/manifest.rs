// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Attachment manifest: the CBOR descriptor carried inside `Kind::File`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

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
        ciborium::into_writer(self, &mut buf).map_err(|e| CoreError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Decode + validate a manifest. Rejects an unknown `manifest_version`.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self> {
        let m: AttachmentManifest =
            ciborium::from_reader(bytes).map_err(|e| CoreError::CborDecode(e.to_string()))?;
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
/// components, ASCII control characters, AND Unicode bidi/format controls
/// (the `invoice\u{202E}cod.exe` right-to-left-override spoofing vector); a
/// blank/empty result falls back to "attachment".
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim_matches('.');
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_or_format_control(*c) && *c != '/' && *c != '\\')
        .collect();
    if cleaned.trim().is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}

/// Unicode bidi/format controls that `char::is_control` (C0/C1 only) misses but
/// which enable filename-display spoofing. Covers the bidi overrides/embeddings
/// (U+202A–202E), the isolates (U+2066–2069), and LRM/RLM (U+200E/200F).
fn is_bidi_or_format_control(c: char) -> bool {
    matches!(c,
        '\u{200E}' | '\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> AttachmentManifest {
        AttachmentManifest {
            manifest_version: crate::attachment::MANIFEST_VERSION,
            attachment_id: [0x11; 16],
            filename: "photo.jpg".into(),
            mime: "image/jpeg".into(),
            total_size: 1000,
            chunk_size: 262_144,
            file_key: [0x22; 32],
            chunks: vec![ChunkRef {
                index: 0,
                ciphertext_hash: [0x33; 32],
                len: 1016,
            }],
        }
    }

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
            chunks: vec![ChunkRef {
                index: 0,
                ciphertext_hash: [0x33; 32],
                len: 1016,
            }],
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
        assert!(matches!(
            err,
            CoreError::Attachment(AttachmentErrorKind::ManifestInvalid(_))
        ));
    }

    #[test]
    fn sanitize_filename_strips_path_and_control() {
        assert_eq!(super::sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(super::sanitize_filename("a/b\\c.txt"), "c.txt");
        assert_eq!(super::sanitize_filename(""), "attachment");
    }

    #[test]
    fn sanitize_filename_strips_bidi_override_and_blank() {
        // RIGHT-TO-LEFT OVERRIDE (U+202E) spoof: "invoice\u{202E}cod.exe" must
        // not retain the override char that flips the displayed extension.
        let spoof = super::sanitize_filename("invoice\u{202E}cod.exe");
        assert!(
            !spoof.contains('\u{202E}'),
            "bidi override must be stripped"
        );
        assert_eq!(spoof, "invoicecod.exe");
        // Whitespace-only / control-only names fall back to "attachment".
        assert_eq!(super::sanitize_filename("   "), "attachment");
        assert_eq!(super::sanitize_filename("\u{202A}\u{202C}"), "attachment");
    }
}
