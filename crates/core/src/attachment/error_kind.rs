// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Typed attachment failures (payload of `CoreError::Attachment`).
//!
//! No `ts_rs::TS` export (unlike the wire-facing subsystem error enums): the
//! attachment manifest is opaque `Vec<u8>` CBOR inside `Kind::File`, so these
//! variants never cross the UI wire boundary.

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
