// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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

use crate::error::Result;
use crate::identity::IdentityKey;

/// On-disk encrypted identity container.
///
/// Opaque type: all state is behind the file handle; callers interact
/// via [`Vault::create`], [`Vault::open`], and [`Vault::change_passphrase`].
#[derive(Debug)]
pub struct Vault {
    // Intentionally opaque. Fields are private and populated by the
    // constructors below.
    _private: (),
}

impl Vault {
    /// Create a new vault at `path`, encrypting `identity` under `passphrase`.
    ///
    /// Fails if the file already exists — callers must delete the old
    /// vault first (explicit user intent).
    pub fn create(_path: &Path, _identity: IdentityKey, _passphrase: &str) -> Result<Self> {
        todo!("Argon2id(passphrase) → XChaCha20-Poly1305 encrypt; write CBOR container")
    }

    /// Open an existing vault, decrypting with `passphrase`.
    pub fn open(_path: &Path, _passphrase: &str) -> Result<(Self, IdentityKey)> {
        todo!("parse CBOR, Argon2id(passphrase), AEAD decrypt, return (Vault, IdentityKey)")
    }

    /// Re-encrypt the vault under a new passphrase, atomically.
    pub fn change_passphrase(&mut self, _old: &str, _new: &str) -> Result<()> {
        todo!("decrypt with old, re-encrypt with new, swap file via temp + rename")
    }
}
