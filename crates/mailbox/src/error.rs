// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Error taxonomy for the mailbox server.
//!
//! Six subsystem sub-enums under one top-level [`MailboxError`]; the
//! `kind()` projection is a structural match (no `str::contains`).
//! See Phase 1.H §error.rs for the pattern this mirrors.

use skattr_core::mailbox::protocol::ErrorCode;
use thiserror::Error;

/// Top-level mailbox error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MailboxError {
    /// SQLite or migration problem.
    #[error("storage: {0}")]
    Storage(#[from] StorageErrorKind),
    /// Auth: hash mismatch, bad signature, expired nonce.
    #[error("auth: {0}")]
    Auth(#[from] AuthErrorKind),
    /// Operator policy violation: too large, TTL bounds, rate limit, cap.
    #[error("policy: {0}")]
    Policy(#[from] PolicyErrorKind),
    /// Wire-level: codec, framing, CBOR.
    #[error("transport: {0}")]
    Transport(#[from] TransportErrorKind),
    /// Configuration parsing or validation.
    #[error("config: {0}")]
    Config(#[from] ConfigErrorKind),
    /// I/O or runtime error not otherwise classified.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Stable subsystem tag for log filters and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxErrorKind {
    /// [`MailboxError::Storage`].
    Storage,
    /// [`MailboxError::Auth`].
    Auth,
    /// [`MailboxError::Policy`].
    Policy,
    /// [`MailboxError::Transport`].
    Transport,
    /// [`MailboxError::Config`].
    Config,
    /// [`MailboxError::Io`].
    Io,
}

impl MailboxError {
    /// Project this error to its subsystem tag.
    #[must_use]
    pub fn kind(&self) -> MailboxErrorKind {
        match self {
            MailboxError::Storage(_) => MailboxErrorKind::Storage,
            MailboxError::Auth(_) => MailboxErrorKind::Auth,
            MailboxError::Policy(_) => MailboxErrorKind::Policy,
            MailboxError::Transport(_) => MailboxErrorKind::Transport,
            MailboxError::Config(_) => MailboxErrorKind::Config,
            MailboxError::Io(_) => MailboxErrorKind::Io,
        }
    }

    /// Map an internal error onto a wire [`ErrorCode`]. Errors that
    /// don't have a stable mapping fold to [`ErrorCode::Internal`].
    #[must_use]
    pub fn to_wire_code(&self) -> ErrorCode {
        match self {
            MailboxError::Auth(AuthErrorKind::HashMismatch) => ErrorCode::HashMismatch,
            MailboxError::Auth(AuthErrorKind::InvalidSignature) => ErrorCode::InvalidSignature,
            MailboxError::Auth(AuthErrorKind::NonceExpired) => ErrorCode::NonceExpired,
            MailboxError::Policy(PolicyErrorKind::TooLarge) => ErrorCode::TooLarge,
            MailboxError::Policy(PolicyErrorKind::TtlTooLong) => ErrorCode::TtlTooLong,
            MailboxError::Policy(PolicyErrorKind::TtlTooShort) => ErrorCode::TtlTooShort,
            MailboxError::Policy(PolicyErrorKind::RateLimited) => ErrorCode::RateLimited,
            MailboxError::Policy(PolicyErrorKind::RecipientFull) => ErrorCode::RecipientFull,
            MailboxError::Storage(StorageErrorKind::NotFound) => ErrorCode::NotFound,
            MailboxError::Transport(TransportErrorKind::DecodeFailed(_)) => {
                ErrorCode::MalformedRequest
            }
            MailboxError::Transport(TransportErrorKind::UnsupportedVersion) => {
                ErrorCode::UnsupportedVersion
            }
            _ => ErrorCode::Internal,
        }
    }
}

/// Storage failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StorageErrorKind {
    /// Migration ran but didn't reach the expected schema_version.
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    /// SQL prepare / step / execute returned an error.
    #[error("sqlite: {0}")]
    Sqlite(String),
    /// `deposit_id` not present.
    #[error("not found")]
    NotFound,
    /// The internal mutex over the SQLite connection was poisoned by
    /// a panicking thread. The connection is unsafe to reuse; treat
    /// as a fatal storage error.
    #[error("connection mutex poisoned")]
    Poisoned,
}

impl From<rusqlite::Error> for StorageErrorKind {
    fn from(e: rusqlite::Error) -> Self {
        StorageErrorKind::Sqlite(e.to_string())
    }
}

impl From<rusqlite::Error> for MailboxError {
    fn from(e: rusqlite::Error) -> Self {
        MailboxError::Storage(StorageErrorKind::Sqlite(e.to_string()))
    }
}

/// Authentication failures (signature, nonce, hash binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthErrorKind {
    /// `sha256(identity_pubkey) != identity_hash`.
    #[error("hash mismatch")]
    HashMismatch,
    /// Ed25519 verification failed.
    #[error("invalid signature")]
    InvalidSignature,
    /// Nonce unknown or older than 30 s.
    #[error("nonce expired")]
    NonceExpired,
}

/// Operator-policy rejections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PolicyErrorKind {
    /// Ciphertext above `max_deposit_size`.
    #[error("too large")]
    TooLarge,
    /// `ttl_request > max_ttl_secs`.
    #[error("ttl too long")]
    TtlTooLong,
    /// `ttl_request != 0 && ttl_request < min_ttl_secs`.
    #[error("ttl too short")]
    TtlTooShort,
    /// Per-conn or global token bucket exhausted.
    #[error("rate limited")]
    RateLimited,
    /// Recipient cap reached, no expired rows available to evict.
    #[error("recipient full")]
    RecipientFull,
}

/// Wire-level failures (codec, framing).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportErrorKind {
    /// Encoding to bytes failed.
    #[error("encode failed: {0}")]
    EncodeFailed(String),
    /// Decoding from bytes failed.
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    /// Frame `version != PROTOCOL_VERSION`.
    #[error("unsupported version")]
    UnsupportedVersion,
}

/// Config / TOML failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigErrorKind {
    /// Could not read the config file.
    #[error("io: {0}")]
    Io(String),
    /// Could not parse TOML.
    #[error("parse: {0}")]
    Parse(String),
    /// A required field was missing or out of range.
    #[error("invalid: {0}")]
    Invalid(String),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_each_top_level_variant() {
        assert_eq!(
            MailboxError::Auth(AuthErrorKind::HashMismatch).kind(),
            MailboxErrorKind::Auth
        );
        assert_eq!(
            MailboxError::Policy(PolicyErrorKind::TooLarge).kind(),
            MailboxErrorKind::Policy
        );
        assert_eq!(
            MailboxError::Storage(StorageErrorKind::NotFound).kind(),
            MailboxErrorKind::Storage
        );
        assert_eq!(
            MailboxError::Transport(TransportErrorKind::UnsupportedVersion).kind(),
            MailboxErrorKind::Transport
        );
        assert_eq!(
            MailboxError::Config(ConfigErrorKind::Invalid("x".into())).kind(),
            MailboxErrorKind::Config
        );
    }

    #[test]
    fn to_wire_code_covers_every_typed_failure() {
        let cases: &[(MailboxError, ErrorCode)] = &[
            (
                MailboxError::Auth(AuthErrorKind::HashMismatch),
                ErrorCode::HashMismatch,
            ),
            (
                MailboxError::Auth(AuthErrorKind::InvalidSignature),
                ErrorCode::InvalidSignature,
            ),
            (
                MailboxError::Auth(AuthErrorKind::NonceExpired),
                ErrorCode::NonceExpired,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TooLarge),
                ErrorCode::TooLarge,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TtlTooLong),
                ErrorCode::TtlTooLong,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::TtlTooShort),
                ErrorCode::TtlTooShort,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::RateLimited),
                ErrorCode::RateLimited,
            ),
            (
                MailboxError::Policy(PolicyErrorKind::RecipientFull),
                ErrorCode::RecipientFull,
            ),
            (
                MailboxError::Storage(StorageErrorKind::NotFound),
                ErrorCode::NotFound,
            ),
            (
                MailboxError::Transport(TransportErrorKind::DecodeFailed("bad".into())),
                ErrorCode::MalformedRequest,
            ),
            (
                MailboxError::Transport(TransportErrorKind::UnsupportedVersion),
                ErrorCode::UnsupportedVersion,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_wire_code(), *expected, "mapping for {err:?}");
        }
    }

    #[test]
    fn unknown_failures_fold_to_internal() {
        let e = MailboxError::Storage(StorageErrorKind::MigrationFailed("oops".into()));
        assert_eq!(e.to_wire_code(), ErrorCode::Internal);
    }
}
