// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Error taxonomy for `skattr-core`.
//!
//! All library functions return [`Result<T>`]. The [`CoreError`] enum is
//! deliberately narrow — subsystems attach their own detail via the
//! `#[from]` conversions below. If you find yourself reaching for a
//! generic `Other` variant, add a new typed variant instead.

use std::io;

use thiserror::Error;

/// Library-wide result alias with [`CoreError`] as the error type.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Top-level error for the `skattr-core` library.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Identity / vault / seed-derivation problem.
    #[error("identity: {0}")]
    Identity(String),

    /// Transport-layer problem (Tor, Noise).
    #[error("transport: {0}")]
    Transport(String),

    /// MLS protocol problem (ciphersuite, keystore, state machine).
    #[error("mls: {0}")]
    Mls(String),

    /// Invite-link parsing or signature verification problem.
    #[error("invite: {0}")]
    Invite(String),

    /// Contact / ContactCard / rotation problem.
    #[error("contact: {0}")]
    Contact(String),

    /// Mailbox wire protocol problem.
    #[error("mailbox: {0}")]
    Mailbox(String),

    /// Delivery (outbox, retry, dedup) problem.
    #[error("delivery: {0}")]
    Delivery(String),

    /// Storage / migration / serialization problem.
    #[error("storage: {0}")]
    Storage(String),

    /// Frame codec (length-prefix, type byte, payload parse) problem.
    #[error("frame codec: {0}")]
    Frame(String),

    /// Configuration problem (bad TOML, missing directory, etc.).
    #[error("config: {0}")]
    Config(String),

    /// Underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// SQLite problem.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// CBOR encode problem.
    #[error("cbor encode: {0}")]
    CborEncode(String),

    /// CBOR decode problem.
    #[error("cbor decode: {0}")]
    CborDecode(String),
}

impl CoreError {
    /// Project this library error onto the stable [`crate::daemon::error_kind::DaemonErrorKind`]
    /// wire enum. Returns `None` when the error has no specific category
    /// the CLI can act on — the IPC layer turns those into
    /// `IpcError::Internal` and logs the full `CoreError` server-side.
    ///
    /// Matching is string-based for now because library error payloads
    /// are free-form `String`s rather than structured variants. If this
    /// grows unwieldy, Phase 2 can restructure the subsystem error
    /// strings into dedicated sub-enums with `thiserror` `#[from]`.
    #[must_use]
    pub fn kind(&self) -> Option<crate::daemon::error_kind::DaemonErrorKind> {
        use crate::daemon::error_kind::DaemonErrorKind as K;
        match self {
            CoreError::Contact(s) if s.contains("not found") => Some(K::ContactNotFound),
            CoreError::Contact(s) if s.contains("ambiguous") => {
                let matches = extract_matches_count(s).unwrap_or(0);
                Some(K::ContactAmbiguous { matches })
            }
            CoreError::Invite(s) if s.contains("expired") => Some(K::InviteExpired),
            CoreError::Invite(s) if s.contains("consumed") => Some(K::InviteConsumed),
            CoreError::Invite(s) if s.contains("signature") => Some(K::InviteSignatureInvalid),
            CoreError::Mls(s) if s.contains("corrupt") => Some(K::GroupCorrupt),
            CoreError::Delivery(s) if s.contains("timeout") => Some(K::DeliveryTimeout),
            CoreError::Transport(s) if s.contains("not ready") || s.contains("bootstrap") => {
                Some(K::TorNotReady)
            }
            CoreError::Sqlite(_) | CoreError::Storage(_) => Some(K::StorageError),
            _ => None,
        }
    }
}

fn extract_matches_count(s: &str) -> Option<u32> {
    // Format expected: "... (N matches)" — find "(N" and parse N.
    let open = s.find('(')? + 1;
    let rest = &s[open..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}
