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

/// Mailbox client (2.B) typed failure reasons.
///
/// Used as the payload of [`CoreError::MailboxClient`]. Callers that produce
/// a `MailboxClientErrorKind` can propagate it with `?` thanks to the
/// `#[from]` impl on the parent enum.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MailboxClientErrorKind {
    /// The mailbox onion service could not be reached.
    #[error("mailbox unreachable")]
    Unreachable,
    /// The server rejected our protocol version.
    #[error("mailbox unsupported protocol version")]
    UnsupportedVersion,
    /// The server applied rate limiting.
    #[error("mailbox rate limited")]
    RateLimited,
    /// The recipient's inbox is full.
    #[error("mailbox recipient full")]
    RecipientFull,
    /// The server rejected our auth signature.
    #[error("mailbox invalid signature")]
    InvalidSignature,
    /// The challenge nonce was expired before we responded.
    #[error("mailbox nonce expired")]
    NonceExpired,
    /// The server sent a frame we could not parse.
    #[error("mailbox malformed response")]
    Malformed,
    /// The received ciphertext hash does not match the deposit receipt.
    #[error("mailbox hash mismatch")]
    HashMismatch,
    /// Catch-all for errors that don't fit the above variants.
    #[error("mailbox: {0}")]
    Other(String),
}

/// Top-level error for the `skattr-core` library.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Identity / vault / seed-derivation problem.
    #[error("identity: {0}")]
    Identity(String),

    /// Transport-layer problem (Tor, Noise).
    #[error("{0}")]
    Transport(#[from] crate::transport::TransportErrorKind),

    /// MLS protocol problem (ciphersuite, keystore, state machine).
    #[error("{0}")]
    Mls(#[from] crate::mls::MlsErrorKind),

    /// Invite-link parsing or signature verification problem.
    #[error("{0}")]
    Invite(#[from] crate::invite::InviteErrorKind),

    /// Contact / ContactCard / rotation problem.
    #[error("{0}")]
    Contact(#[from] crate::contact::ContactErrorKind),

    /// Mailbox wire protocol problem.
    #[error("mailbox: {0}")]
    Mailbox(String),

    /// Mailbox client (2.B) wire-protocol problem.
    #[error("{0}")]
    MailboxClient(#[from] MailboxClientErrorKind),

    /// Delivery (outbox, retry, dedup) problem.
    #[error("{0}")]
    Delivery(#[from] crate::delivery::DeliveryErrorKind),

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
    /// Matching is a pure structural match over typed sub-enums for all six
    /// subsystems (Storage, Contact, Invite, Mls, Delivery, Transport).
    /// No `str::contains` is used — see Phase 1.H item #5.
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
            CoreError::Mls(crate::mls::MlsErrorKind::GroupCorrupt) => Some(K::GroupCorrupt),
            CoreError::Mls(crate::mls::MlsErrorKind::Other(_)) => None,
            CoreError::Delivery(crate::delivery::DeliveryErrorKind::Timeout) => {
                Some(K::DeliveryTimeout)
            }
            CoreError::Delivery(crate::delivery::DeliveryErrorKind::Other(_)) => None,
            CoreError::Transport(crate::transport::TransportErrorKind::TorNotReady) => {
                Some(K::TorNotReady)
            }
            CoreError::Transport(crate::transport::TransportErrorKind::Other(_)) => None,
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
#[allow(clippy::expect_used)]
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

    #[test]
    fn mls_group_corrupt_projects_to_group_corrupt() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Mls(crate::mls::MlsErrorKind::GroupCorrupt);
        assert!(matches!(e.kind(), Some(DaemonErrorKind::GroupCorrupt)));
    }

    #[test]
    fn mls_other_does_not_project() {
        let e = CoreError::Mls(crate::mls::MlsErrorKind::Other("decrypt failed".into()));
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn delivery_timeout_projects_to_delivery_timeout() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Delivery(crate::delivery::DeliveryErrorKind::Timeout);
        assert!(matches!(e.kind(), Some(DaemonErrorKind::DeliveryTimeout)));
    }

    #[test]
    fn delivery_other_does_not_project() {
        let e = CoreError::Delivery(crate::delivery::DeliveryErrorKind::Other("nack".into()));
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn transport_tor_not_ready_projects_to_tor_not_ready() {
        use crate::daemon::error_kind::DaemonErrorKind;
        let e = CoreError::Transport(crate::transport::TransportErrorKind::TorNotReady);
        assert!(matches!(e.kind(), Some(DaemonErrorKind::TorNotReady)));
    }

    #[test]
    fn transport_other_does_not_project() {
        let e = CoreError::Transport(crate::transport::TransportErrorKind::Other(
            "connect refused".into(),
        ));
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn mailbox_client_kind_round_trips_to_none() {
        use crate::error::MailboxClientErrorKind;
        let e = CoreError::MailboxClient(MailboxClientErrorKind::RateLimited);
        // No DaemonErrorKind mapping — IPC layer uses InvalidArgument or events.
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn mailbox_client_other_carries_message() {
        use crate::error::MailboxClientErrorKind;
        let e = CoreError::MailboxClient(MailboxClientErrorKind::Other("disk full".into()));
        let s = format!("{e}");
        assert!(s.contains("disk full"), "got: {s}");
    }

    #[test]
    fn mailbox_client_variants_are_distinct() {
        use crate::error::MailboxClientErrorKind as K;
        assert_ne!(K::Unreachable, K::UnsupportedVersion);
        assert_ne!(K::RateLimited, K::RecipientFull);
        assert_ne!(K::Malformed, K::HashMismatch);
    }

    #[test]
    fn kind_has_no_string_matching() {
        // Phase 1.H item #5: CoreError::kind() must not use str::contains.
        // All subsystem errors project via typed sub-enum variants. If this
        // test fails, someone re-introduced string matching — they should
        // add a typed variant to the offending subsystem's *ErrorKind enum
        // instead.
        const SRC: &str = include_str!("error.rs");
        let fn_start = SRC.find("pub fn kind").expect("kind() in source");
        // Advance past the declaration line, then find the next `fn ` to
        // delimit the body of kind() precisely — the test module follows it.
        let after_decl = fn_start + "pub fn kind".len();
        let next_fn = SRC[after_decl..]
            .find("\n    fn ")
            .map(|off| after_decl + off)
            .unwrap_or(SRC.len());
        let kind_body = &SRC[fn_start..next_fn];
        assert!(
            !kind_body.contains(".contains("),
            "CoreError::kind() must not call str::contains — use typed sub-enums instead"
        );
    }
}
