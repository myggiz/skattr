// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 3.D attachment UI-shell commands: manifest decode, file stat,
//! and open/reveal of received files. Presentation-only — no protocol.

use serde::Serialize;

use skattr_core::AttachmentManifest;

/// Display-only projection of an [`AttachmentManifest`] for the file bubble.
/// Not a ts-rs/core type — lives entirely in the UI shell.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    /// Hex-encoded 16-byte attachment id (matches `hex16ToString` keys).
    pub attachment_id: String,
    /// Sanitized filename from the manifest.
    pub filename: String,
    /// MIME type (post metadata-strip).
    pub mime: String,
    /// Plaintext total size in bytes.
    pub total_size: u64,
}

/// Decode a `Kind::File` manifest (raw CBOR bytes as serialized by serde_json:
/// a JSON number array) into the four scalar display fields. Rejects unknown
/// manifest versions via the canonical core decoder.
#[tauri::command]
pub async fn decode_attachment_manifest(manifest: Vec<u8>) -> Result<ManifestSummary, String> {
    let m = AttachmentManifest::from_cbor(&manifest)
        .map_err(|e| format!("decode manifest: {e}"))?;
    Ok(ManifestSummary {
        attachment_id: m.attachment_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        filename: m.filename,
        mime: m.mime,
        total_size: m.total_size,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decode_roundtrips_a_real_manifest() {
        // Build a real manifest via the public struct literal (all fields are pub).
        // MANIFEST_VERSION = 1 (the only known version accepted by from_cbor).
        let manifest = AttachmentManifest {
            manifest_version: 1,
            attachment_id: [0xab; 16],
            filename: "photo.jpg".to_string(),
            mime: "image/jpeg".to_string(),
            total_size: 1234,
            chunk_size: 49152,
            file_key: [0u8; 32],
            chunks: vec![],
        };
        let bytes = manifest.to_cbor().unwrap();
        let summary = decode_attachment_manifest(bytes).await.unwrap();
        assert_eq!(summary.attachment_id, "ab".repeat(16));
        assert_eq!(summary.filename, "photo.jpg");
        assert_eq!(summary.mime, "image/jpeg");
        assert_eq!(summary.total_size, 1234);
    }

    #[tokio::test]
    async fn decode_rejects_garbage() {
        let err = decode_attachment_manifest(vec![0xff, 0x00, 0x13])
            .await
            .unwrap_err();
        assert!(err.contains("decode manifest"));
    }
}
