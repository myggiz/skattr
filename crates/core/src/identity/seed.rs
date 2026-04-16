// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! BIP39 seed phrases and 32-byte seed material.
//!
//! The [`Seed`] is the user's root of trust. It is displayed to the user
//! as a 24-word [`Mnemonic`] during onboarding and is the only input
//! needed to restore an identity.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Result;

/// A 32-byte seed, used as HKDF input for identity key derivation.
///
/// Zeros on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Seed {
    bytes: [u8; 32],
}

impl Seed {
    /// Generate a fresh 32-byte seed from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Ok(Self { bytes })
    }

    /// Render as a BIP39 24-word mnemonic.
    pub fn to_mnemonic(&self) -> Result<Mnemonic> {
        let _ = &self.bytes;
        todo!("encode 32 bytes as BIP39 (wordlist=English)")
    }

    /// Recover a seed from a 24-word BIP39 mnemonic.
    ///
    /// Validates the checksum; returns an error on any malformed phrase.
    pub fn from_mnemonic(_mnemonic: &Mnemonic) -> Result<Self> {
        todo!("decode BIP39 with checksum validation")
    }

    /// Borrow the raw seed bytes (crate-only: callers outside identity/ should
    /// go through HKDF derivation, not the raw seed).
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

/// A BIP39 mnemonic sequence (typically 24 words).
///
/// Stored as a list of words rather than a single string so callers can
/// format it without implicit trimming or casing decisions.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Mnemonic {
    words: Vec<String>,
}

impl Mnemonic {
    /// Build from an explicit list of words.
    #[must_use]
    pub fn from_words(words: Vec<String>) -> Self {
        Self { words }
    }

    /// Borrow the word list.
    #[must_use]
    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// Parse a mnemonic from whitespace-separated input.
    ///
    /// Performs lower-casing and trim; does **not** validate the BIP39
    /// checksum — that happens in [`Seed::from_mnemonic`].
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let words = input
            .split_whitespace()
            .map(|w| w.to_ascii_lowercase())
            .collect();
        Self { words }
    }
}
