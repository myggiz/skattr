// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Passphrase-encrypted on-disk container for the identity private key.
//!
//! File format (CBOR):
//!
//! ```text
//! { version: u8, kdf_params: { m, t, p, salt }, nonce: [u8; 24], ciphertext: bytes }
//! ```
//!
//! KDF: Argon2id, parameters `m=64 MiB, t=3, p=4`. Output is a 32-byte key
//! fed to XChaCha20-Poly1305. Any bit-flip in the ciphertext is detected
//! by the AEAD tag and surfaces as a typed authentication error.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::IdentityKey;

/// On-disk vault format version. Bumped only via an ADR.
pub const VAULT_VERSION: u8 = 1;

/// AEAD associated-data binding the ciphertext to this exact format version.
const VAULT_AAD: &[u8] = b"skattr-vault-v1";

/// Argon2id parameters baked into the vault file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KdfParams {
    /// Memory cost in KiB.
    #[serde(rename = "m_kib")]
    pub m_kib: u32,
    /// Iteration count (passes).
    #[serde(rename = "t")]
    pub t: u32,
    /// Parallelism (lanes).
    #[serde(rename = "p")]
    pub p: u32,
}

impl KdfParams {
    /// The canonical parameters (`m=64 MiB, t=3, p=4`).
    pub(crate) const fn canonical() -> Self {
        Self { m_kib: 64 * 1024, t: 3, p: 4 }
    }
}

/// CBOR wire form of the vault file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VaultFile {
    /// Format version.
    #[serde(rename = "v")]
    pub v: u8,
    /// KDF parameters that were used.
    #[serde(rename = "kdf")]
    pub kdf: KdfParams,
    /// Per-vault Argon2id salt.
    #[serde(rename = "salt")]
    pub salt: [u8; 16],
    /// XChaCha20-Poly1305 nonce (24 bytes).
    #[serde(rename = "nonce")]
    pub nonce: [u8; 24],
    /// AEAD ciphertext of the 32-byte identity secret (with 16-byte tag).
    #[serde(rename = "ciphertext")]
    pub ciphertext: Vec<u8>,
}

/// On-disk encrypted identity container.
#[derive(Debug)]
pub struct Vault {
    // Path we opened; used by change_passphrase to rewrite atomically.
    path: std::path::PathBuf,
}

impl Vault {
    /// Create a new vault at `path`, encrypting `identity` under `passphrase`.
    pub fn create(_path: &Path, _identity: IdentityKey, _passphrase: &str) -> Result<Self> {
        todo!("Task 10")
    }

    /// Open an existing vault, decrypting with `passphrase`.
    pub fn open(_path: &Path, _passphrase: &str) -> Result<(Self, IdentityKey)> {
        todo!("Task 11")
    }

    /// Re-encrypt the vault under a new passphrase, atomically.
    pub fn change_passphrase(&mut self, _old: &str, _new: &str) -> Result<()> {
        todo!("Task 13")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_file_cbor_roundtrips() {
        let v = VaultFile {
            v: VAULT_VERSION,
            kdf: KdfParams { m_kib: 65536, t: 3, p: 4 },
            salt: [0xA5; 16],
            nonce: [0x5A; 24],
            ciphertext: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&v, &mut buf).unwrap();
        let back: VaultFile = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.v, v.v);
        assert_eq!(back.kdf.m_kib, v.kdf.m_kib);
        assert_eq!(back.salt, v.salt);
        assert_eq!(back.nonce, v.nonce);
        assert_eq!(back.ciphertext, v.ciphertext);
    }
}
