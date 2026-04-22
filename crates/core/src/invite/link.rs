// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Invite link parsing, generation, signing, and verification.
//!
//! Wire layout (fragment-encoded, per design §1.4):
//!
//! ```text
//! skattr://invite/v1#id=<base32(identity_pubkey)>
//!                   &onion=<56-char onion address>
//!                   &kp=<base64url(MLS KeyPackage)>
//!                   &psk=<base64url(32-byte one-time secret)>
//!                   &exp=<unix timestamp>
//!                   &sig=<base64url(Ed25519 signature over canonical CBOR of body)>
//! ```
//!
//! Both `generate` and `from_url` return a validated [`InviteLink`] —
//! the signature is verified before the type is constructed.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};
use crate::storage::KeyPackageRepo;

/// Content that the inviter signs. Deliberately excludes the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteLinkBody {
    /// Inviter's long-term Ed25519 identity.
    pub identity: PublicKey,
    /// Onion service to dial for first contact.
    pub onion: String,
    /// Single-use MLS KeyPackage (binary, TLS-codec bytes from 1.C).
    #[serde(with = "serde_bytes")]
    pub key_package: Vec<u8>,
    /// 32-byte one-time secret mixed into Noise PSK + first MLS Commit.
    pub psk: [u8; 32],
    /// Unix timestamp (seconds) after which the invite is invalid.
    pub expires_at: i64,
}

/// Parsed + verified invite link.
pub struct InviteLink {
    /// Unsigned body fields. `body.psk` is zeroized after parse; read
    /// the PSK via `self.psk` (the Zeroizing guard).
    pub body: InviteLinkBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
    /// Zeroizing copy of the PSK.
    pub psk: InvitePsk,
}

/// A 32-byte one-time secret embedded in an invite.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct InvitePsk(pub [u8; 32]);

impl InviteLink {
    /// Build + sign a new invite.
    pub fn generate(
        _inviter: &IdentityKey,
        _onion: String,
        _key_package: Vec<u8>,
        _psk: [u8; 32],
        _ttl_secs: u64,
        _now: i64,
    ) -> Result<Self> {
        todo!("Task 7")
    }

    /// Parse + verify a `skattr://invite/v1#...` URL.
    pub fn from_url(_url: &str, _now: i64) -> Result<Self> {
        todo!("Task 9")
    }

    /// Re-serialize to a URL.
    pub fn to_url(&self) -> Result<String> {
        todo!("Task 8")
    }

    /// SHA-256 of `body.key_package`.
    pub fn kp_hash(&self) -> [u8; 32] {
        todo!("Task 10")
    }

    /// Record this received invite's KP under `direction='theirs'`.
    pub fn record_received(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        todo!("Task 10")
    }

    /// Whether this invite's KP has been marked consumed in the repo.
    pub fn is_consumed(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<bool> {
        todo!("Task 10")
    }

    /// Flip `consumed=1` for this invite's KP.
    pub fn mark_consumed(&self, _kp_repo: &KeyPackageRepo<'_>) -> Result<()> {
        todo!("Task 10")
    }
}
