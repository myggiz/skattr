// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `ContactCard`: signed, versioned self-published routing record.
//!
//! When a user rotates their onion address or their mailbox list, they
//! publish a new `ContactCard` with a monotonically higher `version`,
//! signed by their identity key. Peers reject cards whose version is
//! not strictly greater than the last verified version for that
//! identity (monotonic replay resistance).

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};

/// A contact's published routing record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCard {
    /// Long-term identity of the card's owner.
    pub identity: PublicKey,
    /// Current onion service, v3 format (56-char base32).
    pub onion: String,
    /// Mailboxes this user is registered with.
    pub mailboxes: Vec<String>,
    /// Monotonic version. Higher is newer.
    pub version: u64,
    /// Unix timestamp after which this card should be considered stale.
    pub expires_at: i64,
    /// Ed25519 signature over the canonical serialization of the above.
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
    ) -> Result<Self> {
        todo!("canonical CBOR, Ed25519 sign")
    }

    /// Verify the card's signature and return the owning pubkey on success.
    pub fn verify(&self) -> Result<PublicKey> {
        todo!("canonical CBOR of body, Ed25519 verify against self.identity")
    }
}
