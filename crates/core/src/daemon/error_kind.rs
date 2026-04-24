// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Stable wire enum projecting rich `CoreError`s onto the IPC surface.
//!
//! The principle: every variant here is an ANSWER a CLI can handle
//! specifically — retry, rephrase the user prompt, pick a different
//! pubkey. `CoreError::kind()` maps as many library errors as we can
//! into these; anything unmapped becomes `IpcError::Internal(..)` on
//! the wire (with the full `CoreError` logged server-side).

use serde::{Deserialize, Serialize};

/// Typed categories a CLI can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DaemonErrorKind {
    /// No contact matches the given pubkey or prefix.
    ContactNotFound,
    /// A prefix matched more than one contact.
    ContactAmbiguous {
        /// Number of contacts that matched the prefix.
        matches: u32,
    },
    /// The invite's `expires_at` is in the past.
    InviteExpired,
    /// The invite's single-use KeyPackage has already been consumed.
    InviteConsumed,
    /// The invite's Ed25519 signature did not verify.
    InviteSignatureInvalid,
    /// MLS group state is unreadable / inconsistent.
    GroupCorrupt,
    /// The daemon's inline wait expired before an ACK arrived; retry
    /// or subscribe to `DeliveryStatusChanged` for the outcome.
    DeliveryTimeout,
    /// Tor is still bootstrapping; retry shortly.
    TorNotReady,
    /// Storage-layer failure that the CLI can't disambiguate further.
    StorageError,
    /// Search query was empty after FTS5 escaping or the engine rejected it.
    SearchSyntax,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    #[test]
    fn contact_not_found_maps() {
        let e = CoreError::Contact("contact: lookup: not found (pubkey=ab…)".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::ContactNotFound));
    }

    #[test]
    fn invite_expired_maps() {
        let e = CoreError::Invite("invite: expired at 1700000000".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteExpired));
    }

    #[test]
    fn invite_consumed_maps() {
        let e = CoreError::Invite("invite: key package already consumed".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::InviteConsumed));
    }

    #[test]
    fn unmapped_returns_none() {
        let e = CoreError::Frame("random framing hiccup".into());
        assert_eq!(e.kind(), None);
    }

    #[test]
    fn delivery_timeout_maps() {
        let e = CoreError::Delivery("delivery: timeout waiting for ACK".into());
        assert_eq!(e.kind(), Some(DaemonErrorKind::DeliveryTimeout));
    }

    #[test]
    fn search_syntax_maps() {
        // FtsSyntax variant projects to SearchSyntax regardless of message content.
        let e = CoreError::Storage(crate::storage::StorageErrorKind::FtsSyntax(
            "fts5: syntax error near '\"'".into(),
        ));
        assert_eq!(e.kind(), Some(DaemonErrorKind::SearchSyntax));

        let e2 = CoreError::Storage(crate::storage::StorageErrorKind::FtsSyntax(
            "malformed MATCH expression".into(),
        ));
        assert_eq!(e2.kind(), Some(DaemonErrorKind::SearchSyntax));

        // Control: plain storage errors still project as StorageError.
        let e3 = CoreError::Storage(crate::storage::StorageErrorKind::Other("disk full".into()));
        assert_eq!(e3.kind(), Some(DaemonErrorKind::StorageError));
    }
}
