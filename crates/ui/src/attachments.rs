// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 3.D attachment UI-shell commands: manifest decode, file stat,
//! and open/reveal of received files. Presentation-only — no protocol.

use std::path::PathBuf;

use serde::Serialize;
use tauri_plugin_opener::OpenerExt;

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
    let m =
        AttachmentManifest::from_cbor(&manifest).map_err(|e| format!("decode manifest: {e}"))?;
    Ok(ManifestSummary {
        attachment_id: m
            .attachment_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        filename: m.filename,
        mime: m.mime,
        total_size: m.total_size,
    })
}

/// Canonicalize a UI-supplied path, assert it is an existing regular file, AND
/// confine it to one of `roots` (downloads or the managed open-cache).
/// A root that fails to canonicalize simply doesn't match — it does not error
/// out the whole check or accept the path.
fn validate_openable(path: &str, roots: &[std::path::PathBuf]) -> Result<PathBuf, String> {
    let canon = std::fs::canonicalize(path).map_err(|e| format!("canonicalize {path}: {e}"))?;
    let ok = roots.iter().any(|r| {
        std::fs::canonicalize(r)
            .map(|cr| canon.starts_with(&cr))
            .unwrap_or(false)
    });
    if !ok {
        return Err(format!("{path}: outside allowed dirs"));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("{path}: not found: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path}: not a regular file"));
    }
    Ok(canon)
}

/// Resolve the effective download directory (configured value, else
/// `~/Downloads`, else `<data_dir>/downloads`) from managed state + the
/// persisted config. Used to confine `open_file`/`reveal_in_folder`.
fn downloads_dir(state: &tauri::State<'_, crate::daemon::AppState>) -> Result<PathBuf, String> {
    let dd = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let mut cfg = match skattr_core::daemon::Config::load(&dd.join("config.toml")) {
        Ok(c) => c,
        Err(_) => {
            skattr_core::daemon::Config::defaults().map_err(|e| format!("config defaults: {e}"))?
        }
    };
    cfg.data_dir = dd;
    Ok(cfg.resolved_download_dir())
}

/// `<data_dir>/cache/open` — the managed decrypt cache created by `OpenAttachment`.
fn open_cache_dir(state: &tauri::State<'_, crate::daemon::AppState>) -> Result<PathBuf, String> {
    let dd = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    Ok(dd.join("cache").join("open"))
}

/// Open a received file with the OS default handler.
#[tauri::command]
pub async fn open_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::daemon::AppState>,
    path: String,
) -> Result<(), String> {
    let roots = vec![downloads_dir(&state)?, open_cache_dir(&state)?];
    let canon = validate_openable(&path, &roots)?;
    app.opener()
        .open_path(canon.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("open_file: {e}"))
}

/// Reveal a received file in the OS file manager.
#[tauri::command]
pub async fn reveal_in_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::daemon::AppState>,
    path: String,
) -> Result<(), String> {
    let roots = vec![downloads_dir(&state)?, open_cache_dir(&state)?];
    let canon = validate_openable(&path, &roots)?;
    app.opener()
        .reveal_item_in_dir(canon)
        .map_err(|e| format!("reveal_in_folder: {e}"))
}

/// Stat a local file and return its byte length. Used by the pre-send size
/// gate. Rejects non-existent paths and non-regular files (dirs, symdirs).
#[tauri::command]
pub async fn file_size(path: String) -> Result<u64, String> {
    let meta = std::fs::metadata(&path).map_err(|e| format!("file_size {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("file_size {path}: not a regular file"));
    }
    Ok(meta.len())
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

    #[tokio::test]
    async fn file_size_reports_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.bin");
        std::fs::write(&p, vec![7u8; 99]).unwrap();
        let n = file_size(p.to_string_lossy().to_string()).await.unwrap();
        assert_eq!(n, 99);
    }

    #[tokio::test]
    async fn file_size_errors_on_missing() {
        let err = file_size("/no/such/file/xyz".to_string())
            .await
            .unwrap_err();
        assert!(err.contains("file_size"));
    }

    #[tokio::test]
    async fn file_size_errors_on_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = file_size(dir.path().to_string_lossy().to_string())
            .await
            .unwrap_err();
        assert!(err.contains("not a regular file"));
    }

    #[test]
    fn validate_inside_downloads_ok() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().to_path_buf();
        let p = downloads.join("f.txt");
        std::fs::write(&p, b"hi").unwrap();
        let got = validate_openable(&p.to_string_lossy(), &[downloads]).unwrap();
        assert!(got.is_absolute());
    }

    #[test]
    fn validate_outside_downloads_errs() {
        let downloads = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = other.path().join("f.txt");
        std::fs::write(&p, b"hi").unwrap();
        let err =
            validate_openable(&p.to_string_lossy(), &[downloads.path().to_path_buf()]).unwrap_err();
        assert!(err.contains("outside allowed dirs"));
    }

    #[test]
    fn validate_missing_file_errs() {
        let downloads = tempfile::tempdir().unwrap();
        let err = validate_openable("/no/such/zzz", &[downloads.path().to_path_buf()]).unwrap_err();
        assert!(err.contains("not found") || err.contains("canonicalize"));
    }

    #[test]
    fn validate_directory_errs() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_openable(&dir.path().to_string_lossy(), &[dir.path().to_path_buf()])
            .unwrap_err();
        assert!(err.contains("not a regular file"));
    }

    #[test]
    fn validate_openable_accepts_cache_and_rejects_outside() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");
        let cache = dir.path().join("cache").join("open").join("aa");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let f = cache.join("x.bin");
        std::fs::write(&f, b"x").unwrap();
        // In-cache file is accepted.
        let cache_open_root = cache.parent().unwrap().parent().unwrap().join("open");
        assert!(
            validate_openable(f.to_str().unwrap(), &[downloads.clone(), cache_open_root]).is_ok()
        );
        // A file outside both roots is rejected.
        let outside = dir.path().join("outside.bin");
        std::fs::write(&outside, b"x").unwrap();
        assert!(validate_openable(outside.to_str().unwrap(), &[downloads, cache]).is_err());
    }
}
