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

use base32::Alphabet;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

const URL_PREFIX: &str = "skattr://invite/v1#";

fn encode_b32(bytes: &[u8]) -> String {
    base32::encode(Alphabet::Rfc4648Lower { padding: false }, bytes)
}

#[allow(dead_code)]
fn decode_b32(s: &str) -> Option<Vec<u8>> {
    base32::decode(Alphabet::Rfc4648Lower { padding: false }, s)
}

fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

#[allow(dead_code)]
fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    // Tolerate accidental padding: strip trailing '=' before decoding.
    let trimmed = s.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(trimmed.as_bytes()).ok()
}

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
        inviter: &IdentityKey,
        onion: String,
        key_package: Vec<u8>,
        psk: [u8; 32],
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self> {
        let expires_at =
            now.checked_add(i64::try_from(ttl_secs).map_err(|_| {
                crate::error::CoreError::Invite("invite: ttl overflows i64".into())
            })?)
            .ok_or_else(|| {
                crate::error::CoreError::Invite("invite: expires_at overflows i64".into())
            })?;

        let body = InviteLinkBody {
            identity: inviter.public(),
            onion,
            key_package,
            psk,
            expires_at,
        };
        let signature = inviter
            .sign_cbor(&body)
            .map_err(|e| crate::error::CoreError::Invite(format!("invite: sign: {e}")))?;
        Ok(Self {
            body,
            signature,
            psk: InvitePsk(psk),
        })
    }

    /// Parse + verify a `skattr://invite/v1#...` URL.
    pub fn from_url(_url: &str, _now: i64) -> Result<Self> {
        todo!("Task 9")
    }

    /// Re-serialize to a URL.
    pub fn to_url(&self) -> Result<String> {
        let id = encode_b32(&self.body.identity.0);
        let kp = encode_b64url(&self.body.key_package);
        let psk = encode_b64url(&self.body.psk);
        let sig = encode_b64url(&self.signature.0);
        Ok(format!(
            "{prefix}id={id}&onion={onion}&kp={kp}&psk={psk}&exp={exp}&sig={sig}",
            prefix = URL_PREFIX,
            id = id,
            onion = self.body.onion,
            kp = kp,
            psk = psk,
            exp = self.body.expires_at,
            sig = sig,
        ))
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixed_kp() -> Vec<u8> {
        (0..64u8).collect()
    }

    #[test]
    fn generate_populates_body_and_signature() {
        let inviter = IdentityKey::generate().unwrap();
        let psk = [0xAA; 32];
        let invite = InviteLink::generate(
            &inviter,
            "abc.onion".into(),
            fixed_kp(),
            psk,
            3600,
            1_000_000,
        )
        .unwrap();

        assert_eq!(invite.body.identity, inviter.public());
        assert_eq!(invite.body.onion, "abc.onion");
        assert_eq!(invite.body.key_package, fixed_kp());
        assert_eq!(invite.body.psk, psk);
        assert_eq!(invite.body.expires_at, 1_000_000 + 3600);
        assert_eq!(invite.signature.0.len(), 64);
        assert_eq!(invite.psk.0, psk);
    }

    #[test]
    fn generate_signature_verifies_via_identity() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "xyz.onion".into(),
            fixed_kp(),
            [0xBB; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        IdentityKey::verify_cbor(&invite.body.identity, &invite.body, &invite.signature)
            .expect("body signature must verify against its embedded identity");
    }

    #[test]
    fn to_url_has_expected_prefix_and_all_six_params() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "abc.onion".into(),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();

        let url = invite.to_url().unwrap();
        assert!(url.starts_with("skattr://invite/v1#"));
        let fragment = url.strip_prefix("skattr://invite/v1#").unwrap();
        let keys: Vec<&str> = fragment
            .split('&')
            .map(|p| p.split('=').next().unwrap())
            .collect();
        assert_eq!(keys, &["id", "onion", "kp", "psk", "exp", "sig"]);
    }

    #[test]
    fn to_url_is_deterministic() {
        let inviter = IdentityKey::generate().unwrap();
        let invite = InviteLink::generate(
            &inviter,
            "a.onion".into(),
            fixed_kp(),
            [0xAA; 32],
            3600,
            1_000_000,
        )
        .unwrap();
        assert_eq!(invite.to_url().unwrap(), invite.to_url().unwrap());
    }
}
