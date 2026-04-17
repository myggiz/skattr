// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Ed25519 identity keys, public keys, and detached signatures.

use core::fmt;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CoreError, Result};

/// An Ed25519 public key, 32 bytes.
///
/// `PublicKey` is `Copy` and safe to log in debug output at `trace`
/// level only — pubkeys are sensitive metadata. Display format is
/// hex-encoded; see [`PublicKey::to_hex`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// Hex-encoded public key (64 lowercase characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a hex-encoded public key.
    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes =
            hex::decode(s).map_err(|e| CoreError::Identity(format!("invalid hex pubkey: {e}")))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CoreError::Identity("pubkey must be 32 bytes".into()))?;
        Ok(Self(arr))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact by default: full pubkey only surfaces via to_hex().
        write!(f, "PublicKey({}…)", &self.to_hex()[..8])
    }
}

/// A detached Ed25519 signature, 64 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(#[serde(with = "serde_big_array::BigArray")] pub [u8; 64]);

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({}…)", hex::encode(&self.0[..4]))
    }
}

/// The long-term Ed25519 identity keypair.
///
/// Holds the private scalar in memory only for the lifetime of the
/// process (never written to disk except encrypted via [`Vault`]).
/// The secret bytes zero on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IdentityKey {
    /// Raw 32-byte Ed25519 seed (the "secret" half; public key is derived).
    secret: [u8; 32],
}

impl IdentityKey {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut secret = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut secret);
        Ok(Self { secret })
    }

    /// Derive an identity deterministically from a [`Seed`] via HKDF.
    ///
    /// The derivation is domain-separated with the label
    /// `"skattr-identity-v1"`. Changing this label is a wire-incompatible
    /// change — do not do it without an ADR.
    pub fn from_seed(seed: &crate::identity::Seed) -> Result<Self> {
        use crate::identity::derive::{hkdf_expand, INFO_IDENTITY_V1};
        let okm = hkdf_expand::<32>(seed.as_bytes(), INFO_IDENTITY_V1)?;
        Ok(Self::from_bytes(okm))
    }

    /// Public half of the keypair.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        let signing = SigningKey::from_bytes(&self.secret);
        PublicKey(signing.verifying_key().to_bytes())
    }

    /// Sign an arbitrary message.
    pub fn sign(&self, message: &[u8]) -> Signature {
        let signing = SigningKey::from_bytes(&self.secret);
        let sig: ed25519_dalek::Signature = signing.sign(message);
        Signature(sig.to_bytes())
    }

    /// Verify a signature against a pubkey. Constant-time, no panics.
    pub fn verify(pubkey: &PublicKey, message: &[u8], signature: &Signature) -> Result<()> {
        let vk = VerifyingKey::from_bytes(&pubkey.0)
            .map_err(|e| CoreError::Identity(format!("invalid pubkey bytes: {e}")))?;
        let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
        vk.verify_strict(message, &sig)
            .map_err(|_| CoreError::Identity("signature verification failed".into()))
    }

    /// Consume into raw secret bytes. Caller is responsible for zeroization.
    ///
    /// This is only used by the vault on encrypted save; avoid calling
    /// it from other code paths.
    pub(crate) fn into_bytes(mut self) -> [u8; 32] {
        let out = self.secret;
        self.secret.zeroize();
        out
    }

    /// Construct from Zeroizing-wrapped secret bytes. Private: only
    /// callable from inside the crate (vault open, seed derivation).
    ///
    /// Takes `Zeroizing<[u8; 32]>` (not bare `[u8; 32]`) so the caller's
    /// guard drops after the move, leaving `self.secret` as the sole
    /// un-wiped copy — which itself zeroes on drop via the struct's
    /// `ZeroizeOnDrop` derive.
    pub(crate) fn from_bytes(secret: zeroize::Zeroizing<[u8; 32]>) -> Self {
        Self { secret: *secret }
    }
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_is_32_bytes_and_stable() {
        let id = IdentityKey::generate().unwrap();
        let pk1 = id.public();
        let pk2 = id.public();
        assert_eq!(pk1.0.len(), 32);
        assert_eq!(
            pk1, pk2,
            "public() must be deterministic for the same secret"
        );
    }

    #[test]
    fn distinct_secrets_produce_distinct_pubkeys() {
        let a = IdentityKey::generate().unwrap();
        let b = IdentityKey::generate().unwrap();
        assert_ne!(a.public(), b.public());
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let id = IdentityKey::generate().unwrap();
        let msg = b"skattr handshake payload v1";
        let sig = id.sign(msg);
        IdentityKey::verify(&id.public(), msg, &sig).expect("signature must verify");
    }

    #[test]
    fn verify_rejects_tampered_message() {
        let id = IdentityKey::generate().unwrap();
        let sig = id.sign(b"original message");
        let err = IdentityKey::verify(&id.public(), b"tampered message", &sig)
            .expect_err("tampered verify must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn verify_rejects_wrong_pubkey() {
        let signer = IdentityKey::generate().unwrap();
        let other = IdentityKey::generate().unwrap();
        let sig = signer.sign(b"msg");
        IdentityKey::verify(&other.public(), b"msg", &sig)
            .expect_err("verify under wrong pubkey must fail");
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = crate::identity::Seed::generate().unwrap();
        let a = IdentityKey::from_seed(&seed).unwrap();
        let b = IdentityKey::from_seed(&seed).unwrap();
        assert_eq!(a.public(), b.public(), "same seed must yield same pubkey");
    }

    #[test]
    fn from_bytes_accepts_zeroizing() {
        let mut buf = zeroize::Zeroizing::new([0u8; 32]);
        buf[0] = 1;
        let id = IdentityKey::from_bytes(buf);
        assert_eq!(id.public().0.len(), 32);
    }

    #[test]
    fn from_seed_is_domain_separated_from_raw_bytes() {
        // If HKDF were accidentally bypassed (e.g. someone rewrote from_seed
        // as Self::from_bytes(seed.as_bytes().into())), this test would fail:
        // the "raw-bytes" and "seed-derived" keys would coincide.
        let bytes = [0x42u8; 32];
        let raw_key = IdentityKey::from_bytes(zeroize::Zeroizing::new(bytes));
        let seed = crate::identity::Seed::from_bytes(bytes);
        let derived = IdentityKey::from_seed(&seed).unwrap();
        assert_ne!(
            raw_key.public(),
            derived.public(),
            "from_seed must mix HKDF label; raw-bytes and seed-derived keys must differ"
        );
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let id = IdentityKey::generate().unwrap();
        let msg = b"payload";
        let mut sig = id.sign(msg);
        // Flip the first byte of the signature's R component.
        sig.0[0] ^= 0x01;
        let err = IdentityKey::verify(&id.public(), msg, &sig)
            .expect_err("tampered signature must fail verify_strict");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }
}
