// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Identity: Ed25519 long-term keys, BIP39 seed phrases, encrypted vaults.
//!
//! A Skattr identity is an Ed25519 keypair derived deterministically from
//! a 32-byte seed. The seed is the single "root of trust": it is exposed
//! to the user as a BIP39 mnemonic and is the only secret needed to
//! restore an identity after total data loss.
//!
//! Derivation path:
//!
//! ```text
//! mnemonic → BIP39(seed_bytes) → HKDF("skattr-identity-v1") → ed25519_seed → Keypair
//! ```
//!
//! See also [`vault`] for on-disk storage of the private key.

pub mod derive;
pub mod key;
pub mod seed;
pub mod vault;

pub use key::{IdentityKey, PublicKey, Signature};
pub use seed::{Mnemonic, Seed};
pub use vault::Vault;
