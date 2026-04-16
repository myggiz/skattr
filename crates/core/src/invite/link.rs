// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Invite link parsing, generation, signing, and verification.
//!
//! Wire layout (fragment-encoded):
//!
//! ```text
//! skattr://invite/v1#
//!   id=<base32(identity_pubkey)>
//!   &onion=<56-char onion address>
//!   &kp=<base64url(MLS KeyPackage)>
//!   &psk=<base64url(32-byte one-time secret)>
//!   &exp=<unix timestamp>
//!   &sig=<base64url(Ed25519 signature over the above)>
//! ```

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey, Signature};

/// Parsed and verified invite link.
///
/// Constructing one of these requires a valid signature; field access
/// is therefore safe to trust (modulo caller still checking `expires_at`
/// against local clock).
pub struct InviteLink {
    /// Inviter's long-term Ed25519 identity.
    pub identity: PublicKey,
    /// Onion service to dial for first contact.
    pub onion: String,
    /// Single-use MLS KeyPackage (binary).
    pub key_package: Vec<u8>,
    /// 32-byte PSK mixed into Noise handshake and first MLS Commit.
    pub psk: InvitePsk,
    /// Unix timestamp after which the invite is invalid.
    pub expires_at: i64,
    /// Ed25519 signature over `(identity, onion, key_package, psk, expires_at)`.
    pub signature: Signature,
}

/// A 32-byte one-time secret embedded in an invite.
///
/// Separate newtype so we can enforce zeroize on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct InvitePsk(pub [u8; 32]);

impl InviteLink {
    /// Generate a new invite link signed by `inviter`.
    pub fn generate(
        _inviter: &IdentityKey,
        _onion: String,
        _key_package: Vec<u8>,
        _ttl_secs: u64,
    ) -> Result<Self> {
        todo!("build fields, canonical-serialize, sign with inviter")
    }

    /// Parse and verify a `skattr://invite/v1#...` URL.
    pub fn from_url(_url: &str) -> Result<Self> {
        todo!("split fragment, decode each field, verify Ed25519 signature, check exp")
    }

    /// Serialize back to a `skattr://invite/v1#...` URL.
    pub fn to_url(&self) -> String {
        todo!("reverse of from_url: base32/base64url encode, concat with separators")
    }
}
