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
    #[error("{0}")]
    Invite(#[from] crate::invite::InviteErrorKind),

    /// Contact / ContactCard / rotation problem.
    #[error("{0}")]
    Contact(#[from] crate::contact::ContactErrorKind),

    /// Mailbox wire protocol problem.
    #[error("mailbox: {0}")]
    Mailbox(String),

    /// Delivery (outbox, retry, dedup) problem.
    #[error("delivery: {0}")]
    Delivery(String),

    /// Storage / migration / serialization problem.
    #[error("{0}")]
    Storage(#[from] crate::storage::StorageErrorKind),

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
    /// Matching is structural where subsystems have typed error kinds
    /// (`StorageErrorKind`, `ContactErrorKind`, `InviteErrorKind`).
    /// Remaining subsystems still use free-form `String`s and are matched
    /// via `str::contains` until Phase 1.H sweeps them.
    #[must_use]
    pub fn kind(&self) -> Option<crate::daemon::error_kind::DaemonErrorKind> {
        use crate::daemon::error_kind::DaemonErrorKind as K;
        match self {
            CoreError::Contact(crate::contact::ContactErrorKind::NotFound) => {
                Some(K::ContactNotFound)
            }
            CoreError::Contact(crate::contact::ContactErrorKind::Ambiguous { matches }) => {
                Some(K::ContactAmbiguous { matches: *matches })
            }
            CoreError::Contact(crate::contact::ContactErrorKind::Other(_)) => None,
            CoreError::Invite(crate::invite::InviteErrorKind::Expired) => Some(K::InviteExpired),
            CoreError::Invite(crate::invite::InviteErrorKind::Consumed) => Some(K::InviteConsumed),
            CoreError::Invite(crate::invite::InviteErrorKind::SignatureInvalid) => {
                Some(K::InviteSignatureInvalid)
            }
            CoreError::Invite(crate::invite::InviteErrorKind::Other(_)) => None,
            CoreError::Mls(s) if s.contains("corrupt") => Some(K::GroupCorrupt),
            CoreError::Delivery(s) if s.contains("timeout") => Some(K::DeliveryTimeout),
            CoreError::Transport(s) if s.contains("not ready") || s.contains("bootstrap") => {
                Some(K::TorNotReady)
            }
            CoreError::Storage(crate::storage::StorageErrorKind::FtsSyntax(_)) => {
                Some(K::SearchSyntax)
            }
            CoreError::Storage(crate::storage::StorageErrorKind::DuplicateMessage) => {
                Some(K::StorageError) // Phase 1.H: no dedicated Daemon variant; storage-level signal only
            }
            CoreError::Storage(crate::storage::StorageErrorKind::Other(_))
            | CoreError::Sqlite(_) => Some(K::StorageError),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_not_found_projects_to_contact_not_found() {
        use crate::contact::ContactErrorKind;
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Contact(ContactErrorKind::NotFound);
        assert_eq!(e.kind(), Some(DaemonErrorKind::ContactNotFound));
    }

    #[test]
    fn contact_ambiguous_projects_with_match_count() {
        use crate::contact::ContactErrorKind;
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Contact(ContactErrorKind::Ambiguous { matches: 3 });
        assert!(matches!(
            e.kind(),
            Some(DaemonErrorKind::ContactAmbiguous { matches: 3 })
        ));
    }

    #[test]
    fn contact_other_does_not_project() {
        use crate::contact::ContactErrorKind;
        let e = CoreError::Contact(ContactErrorKind::Other("unknown".into()));
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn storage_fts_syntax_projects_to_search_syntax() {
        use crate::daemon::error_kind::DaemonErrorKind;
        use crate::storage::StorageErrorKind;
        let e = CoreError::Storage(StorageErrorKind::FtsSyntax("near \"foo\"".into()));
        assert_eq!(e.kind(), Some(DaemonErrorKind::SearchSyntax));
    }

    #[test]
    fn storage_duplicate_message_projects_to_storage_error() {
        use crate::daemon::error_kind::DaemonErrorKind;
        use crate::storage::StorageErrorKind;
        let e = CoreError::Storage(StorageErrorKind::DuplicateMessage);
        assert_eq!(e.kind(), Some(DaemonErrorKind::StorageError));
    }

    #[test]
    fn storage_other_projects_to_storage_error() {
        use crate::daemon::error_kind::DaemonErrorKind;
        use crate::storage::StorageErrorKind;
        let e = CoreError::Storage(StorageErrorKind::Other("prepare failed".into()));
        assert_eq!(e.kind(), Some(DaemonErrorKind::StorageError));
    }

    #[test]
    fn invite_expired_projects_to_invite_expired() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Invite(crate::invite::InviteErrorKind::Expired);
        assert!(matches!(e.kind(), Some(DaemonErrorKind::InviteExpired)));
    }

    #[test]
    fn invite_consumed_projects_to_invite_consumed() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Invite(crate::invite::InviteErrorKind::Consumed);
        assert!(matches!(e.kind(), Some(DaemonErrorKind::InviteConsumed)));
    }

    #[test]
    fn invite_signature_invalid_projects_to_signature_invalid() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Invite(crate::invite::InviteErrorKind::SignatureInvalid);
        assert!(matches!(
            e.kind(),
            Some(DaemonErrorKind::InviteSignatureInvalid)
        ));
    }

    #[test]
    fn invite_other_does_not_project() {
        let e = CoreError::Invite(crate::invite::InviteErrorKind::Other("x".into()));
        assert_eq!(e.kind(), None);
    }
}
