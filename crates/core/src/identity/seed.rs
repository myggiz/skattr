// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! BIP39 seed phrases and 32-byte seed material.
//!
//! The [`Seed`] is the user's root of trust. It is displayed to the user
//! as a 24-word [`Mnemonic`] during onboarding and is the only input
//! needed to restore an identity.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CoreError, Result};

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
        let m = bip39::Mnemonic::from_entropy_in(bip39::Language::English, &self.bytes)
            .map_err(|e| CoreError::Identity(format!("bip39 encode: {e}")))?;
        // The joined phrase lives on the heap; wipe it before drop.
        let phrase = zeroize::Zeroizing::new(m.to_string());
        let words: Vec<String> = phrase
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        Ok(Mnemonic { words })
    }

    /// Recover a seed from a 24-word BIP39 mnemonic.
    ///
    /// Validates the checksum; returns an error on any malformed phrase.
    pub fn from_mnemonic(mnemonic: &Mnemonic) -> Result<Self> {
        let phrase = zeroize::Zeroizing::new(mnemonic.words.join(" "));
        let m = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &phrase)
            .map_err(|e| CoreError::Identity(format!("bip39 decode: {e}")))?;
        let entropy = zeroize::Zeroizing::new(m.to_entropy());
        let bytes: [u8; 32] = entropy
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::Identity("seed must be 32 bytes (24-word BIP39)".into()))?;
        Ok(Self { bytes })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_mnemonic_yields_24_words() {
        let seed = Seed::generate().unwrap();
        let mnemonic = seed.to_mnemonic().unwrap();
        assert_eq!(mnemonic.words().len(), 24, "32-byte seed → 24-word BIP39");
    }

    #[test]
    fn known_vector_abandon_x23_art_yields_zero_seed() {
        // BIP39 test vector: 23×"abandon" + "art" encodes 32 zero bytes.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = Mnemonic::parse(phrase);
        let seed = Seed::from_mnemonic(&m).unwrap();
        assert_eq!(seed.as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn mnemonic_roundtrip() {
        for _ in 0..8 {
            let seed = Seed::generate().unwrap();
            let mnemonic = seed.to_mnemonic().unwrap();
            let back = Seed::from_mnemonic(&mnemonic).unwrap();
            assert_eq!(
                seed.as_bytes(),
                back.as_bytes(),
                "round-trip must be identity"
            );
        }
    }

    #[test]
    fn from_mnemonic_rejects_bad_checksum() {
        // All zeros would only checksum-valid if the last word were the magic "art";
        // use 24 copies of "abandon" which fails the BIP39 checksum.
        let bad = Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon abandon",
        );
        let err = Seed::from_mnemonic(&bad)
            .err()
            .expect("bad checksum must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_mnemonic_roundtrip(bytes in prop::array::uniform32(any::<u8>())) {
            let seed = Seed { bytes };
            let mnemonic = seed.to_mnemonic().unwrap();
            let back = Seed::from_mnemonic(&mnemonic).unwrap();
            prop_assert_eq!(seed.as_bytes(), back.as_bytes());
        }
    }
}
