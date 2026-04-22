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
        _signer: &IdentityKey,
        _onion: String,
        _mailboxes: Vec<String>,
        _version: u64,
        _ttl_secs: u64,
        _now: i64,
    ) -> Result<Self> {
        todo!("Task 5")
    }

    /// Verify the Ed25519 signature + expiry. On success returns the
    /// body's `identity` (the caller typically cross-checks against a
    /// known contact).
    pub fn verify(&self, _now: i64) -> Result<PublicKey> {
        todo!("Task 6")
    }
}
