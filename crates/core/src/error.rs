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
