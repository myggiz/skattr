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
    pub fn verify(&self, now: i64) -> Result<PublicKey> {
        if now > self.body.expires_at {
            return Err(crate::error::CoreError::Contact(
                "contact: card: expired".into(),
            ));
        }
        IdentityKey::verify_cbor(&self.body.identity, &self.body, &self.signature).map_err(
            |_| {
                crate::error::CoreError::Contact(
                    "contact: card: signature verification failed".into(),
                )
            },
        )?;
        Ok(self.body.identity)
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

    fn fresh_card(version: u64, expires_at: i64, signer: &IdentityKey) -> ContactCard {
        ContactCard::sign(
            signer,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.onion".into(),
            Vec::new(),
            version,
            (expires_at - 1_000_000).max(1) as u64,
            1_000_000,
        )
        .unwrap()
    }

    #[test]
    fn verify_returns_identity_on_valid_card() {
        let signer = IdentityKey::generate().unwrap();
        let card = fresh_card(1, 1_003_600, &signer);
        let got = card.verify(1_000_000).unwrap();
        assert_eq!(got, signer.public());
    }

    #[test]
    fn verify_rejects_expired() {
        let signer = IdentityKey::generate().unwrap();
        let card = fresh_card(1, 1_003_600, &signer);
        let err = card.verify(1_003_601).expect_err("must reject expired");
        match err {
            crate::error::CoreError::Contact(s) => assert!(s.contains("expired"), "got: {s}"),
            other => panic!("expected Contact, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let signer = IdentityKey::generate().unwrap();
        let mut card = fresh_card(1, 1_003_600, &signer);
        card.signature.0[0] ^= 0xFF;
        let err = card.verify(1_000_000).expect_err("tampered");
        match err {
            crate::error::CoreError::Contact(s) => {
                assert!(s.contains("signature verification failed"), "got: {s}");
            }
            crate::error::CoreError::Identity(s) => {
                assert!(s.contains("verification failed"), "got: {s}");
            }
            other => panic!("expected Contact/Identity, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_wrong_signer() {
        let signer = IdentityKey::generate().unwrap();
        let other = IdentityKey::generate().unwrap();
        let mut card = fresh_card(1, 1_003_600, &signer);
        // Replace the body's identity with the other signer's pubkey —
        // signature was made with `signer`, so verification under
        // `other.public()` must fail.
        card.body.identity = other.public();
        let err = card.verify(1_000_000).expect_err("wrong signer");
        match err {
            crate::error::CoreError::Contact(s) => {
                assert!(s.contains("signature verification failed"), "got: {s}");
            }
            crate::error::CoreError::Identity(s) => {
                assert!(s.contains("verification failed"), "got: {s}");
            }
            other => panic!("expected Contact/Identity, got {other:?}"),
        }
    }
}
