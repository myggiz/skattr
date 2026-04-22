// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `ContactCard`: signed, versioned self-published routing record.
//!
//! When a user rotates their onion address or their mailbox list, they
//! publish a new `ContactCard` with a monotonically higher `version`,
//! signed by their identity key. Peers reject cards whose version is
//! not strictly greater than the last verified version for that
//! identity (monotonic replay resistance) — enforced by
//! [`crate::storage::contacts::ContactRepo::put_card`].

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};

/// Content the owner signs. Excludes the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCardBody {
    /// Long-term identity of the card's owner.
    pub identity: PublicKey,
    /// Current onion service, v3 format (56-char base32).
    pub onion: String,
    /// Mailboxes this user is registered with. Empty in 1.D.
    pub mailboxes: Vec<String>,
    /// Monotonic version. Higher is newer.
    pub version: u64,
    /// Unix timestamp after which this card is considered stale.
    pub expires_at: i64,
}

/// A contact's published routing record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCard {
    /// Unsigned fields.
    pub body: ContactCardBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
}

impl ContactCard {
    /// Build and sign a new card.
    pub fn sign(
        signer: &IdentityKey,
        onion: String,
        mailboxes: Vec<String>,
        version: u64,
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self> {
        let expires_at = now
            .checked_add(i64::try_from(ttl_secs).map_err(|_| {
                crate::error::CoreError::Contact("contact: card: ttl overflows i64".into())
            })?)
            .ok_or_else(|| {
                crate::error::CoreError::Contact("contact: card: expires_at overflows i64".into())
            })?;

        let body = ContactCardBody {
            identity: signer.public(),
            onion,
            mailboxes,
            version,
            expires_at,
        };
        let signature = signer.sign_cbor(&body)?;
        Ok(Self { body, signature })
    }

    /// Verify the Ed25519 signature + expiry. On success returns the
    /// body's `identity` (the caller typically cross-checks against a
    /// known contact).
    pub fn verify(&self, _now: i64) -> Result<PublicKey> {
        todo!("Task 6")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sign_fills_body_fields_and_signature_is_64_bytes() {
        let signer = IdentityKey::generate().unwrap();
        let card = ContactCard::sign(
            &signer,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion".into(),
            Vec::new(),
            7,
            3600,
            1_000_000,
        )
        .unwrap();

        assert_eq!(card.body.identity, signer.public());
        assert_eq!(card.body.version, 7);
        assert_eq!(card.body.expires_at, 1_000_000 + 3600);
        assert!(card.body.mailboxes.is_empty());
        assert_eq!(card.body.onion.len(), 62); // 56-char onion + ".onion"
        assert_eq!(card.signature.0.len(), 64);
    }
}
