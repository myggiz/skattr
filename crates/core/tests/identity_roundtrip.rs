// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end identity round-trip.
//!
//! This is the Phase 0.B exit-criterion test: "wipe data, restore from seed,
//! recomputed PublicKey matches."

use skattr_core::identity::{IdentityKey, Seed, Vault};

#[test]
fn seed_roundtrip_preserves_pubkey() {
    let seed = Seed::generate().unwrap();
    let id1 = IdentityKey::from_seed(&seed).unwrap();
    let pub1 = id1.public();

    // Simulate "wipe data": drop the IdentityKey, keep only the mnemonic.
    let mnemonic = seed.to_mnemonic().unwrap();
    drop(seed);
    drop(id1);

    // "Restore from seed": recover the same pubkey.
    let recovered_seed = Seed::from_mnemonic(&mnemonic).unwrap();
    let id2 = IdentityKey::from_seed(&recovered_seed).unwrap();
    assert_eq!(id2.public(), pub1);
}

#[test]
fn vault_roundtrip_across_process_simulation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("identity.vault");

    // Phase 1: "init"
    let id = IdentityKey::generate().unwrap();
    let original_pub = id.public();
    Vault::create(&path, id, "correct horse battery staple").unwrap();

    // Phase 2: "process restart" — reopen the vault from the filesystem.
    let (_vault, recovered) = Vault::open(&path, "correct horse battery staple").unwrap();
    assert_eq!(recovered.public(), original_pub);

    // Phase 3: change passphrase, reopen under the new one.
    let (mut vault, _) = Vault::open(&path, "correct horse battery staple").unwrap();
    vault
        .change_passphrase("correct horse battery staple", "new passphrase")
        .unwrap();
    let (_, recovered_again) = Vault::open(&path, "new passphrase").unwrap();
    assert_eq!(recovered_again.public(), original_pub);
}
