// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Ed25519 identity keys, public keys, and detached signatures.

use core::fmt;

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
    pub fn from_seed(_seed: &crate::identity::Seed) -> Result<Self> {
        todo!("derive Ed25519 seed via HKDF(seed, \"skattr-identity-v1\")")
    }

    /// Public half of the keypair.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        // Placeholder: real implementation computes Ed25519 public from secret.
        // Kept as a deterministic stub so downstream signatures compile.
        let _ = &self.secret;
        todo!("compute Ed25519 public key from secret scalar")
    }

    /// Sign an arbitrary message.
    pub fn sign(&self, _message: &[u8]) -> Signature {
        let _ = &self.secret;
        todo!("Ed25519 sign")
    }

    /// Verify a signature against a pubkey. Constant-time, no panics.
    pub fn verify(_pubkey: &PublicKey, _message: &[u8], _signature: &Signature) -> Result<()> {
        todo!("Ed25519 verify")
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

    /// Construct from raw secret bytes. Private: only callable from inside
    /// the crate (vault open, seed derivation).
    pub(crate) fn from_bytes(secret: [u8; 32]) -> Self {
        Self { secret }
    }
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityKey(<redacted>)")
    }
}
