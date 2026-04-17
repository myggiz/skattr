// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! V3 hidden-service signing key: generation, age-encrypted persistence,
//! and load.
//!
//! The HS signing key is Ed25519 (v3 onion services use `HsIdKey`, the
//! identity master key). It is generated fresh on first `skattr daemon`,
//! persisted encrypted under a `HKDF("skattr-hs-storage-v1")` derivation
//! of the identity seed, and reloaded each subsequent run. A new HS key
//! means a new `.onion` address — that's deliberate rotation (documented
//! in design §1.1 and the Phase 2 rotation workstream).

use std::path::Path;

use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::derive::{hkdf_expand, INFO_HS_STORAGE_V1};
use crate::identity::Seed;

/// Raw bytes of a v3 HS signing key (Ed25519 secret).
pub(crate) type HsSecretBytes = Zeroizing<[u8; 32]>;

/// Create or load the HS signing key at `path`, deriving the at-rest
/// encryption key from `seed`.
///
/// If the file does not exist, a fresh 32-byte key is generated and
/// written. On subsequent calls at the same path with the same seed,
/// the existing key is decrypted and returned unchanged — same seed
/// + same file → same `.onion` address.
pub(crate) fn load_or_create(path: &Path, seed: &Seed) -> Result<HsSecretBytes> {
    if path.exists() {
        load(path, seed)
    } else {
        let bytes = generate();
        save(path, seed, &bytes)?;
        Ok(bytes)
    }
}

fn generate() -> HsSecretBytes {
    use rand::RngCore;
    let mut out = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(out.as_mut());
    out
}

fn derive_storage_key(seed: &Seed) -> Result<Zeroizing<[u8; 32]>> {
    hkdf_expand::<32>(seed.as_bytes(), INFO_HS_STORAGE_V1)
}

fn save(path: &Path, seed: &Seed, bytes: &[u8; 32]) -> Result<()> {
    let key = derive_storage_key(seed)?;
    let passphrase = age::secrecy::SecretString::from(hex::encode(key.as_ref()));
    let encryptor = age::Encryptor::with_user_passphrase(passphrase);

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Transport(format!("age wrap: {e}")))?;
    use std::io::Write;
    writer
        .write_all(bytes)
        .map_err(|e| CoreError::Transport(format!("age write: {e}")))?;
    writer
        .finish()
        .map_err(|e| CoreError::Transport(format!("age finish: {e}")))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &ciphertext)?;
    Ok(())
}

fn load(path: &Path, seed: &Seed) -> Result<HsSecretBytes> {
    let ciphertext = std::fs::read(path)?;
    let key = derive_storage_key(seed)?;
    let passphrase = age::secrecy::SecretString::from(hex::encode(key.as_ref()));

    // age 0.11: Decryptor is no longer an enum; use scrypt::Identity as the
    // identity and pass it via an iterator to decrypt().
    let decryptor = age::Decryptor::new_buffered(&ciphertext[..])
        .map_err(|e| CoreError::Transport(format!("age decryptor: {e}")))?;

    if !decryptor.is_scrypt() {
        return Err(CoreError::Transport("unexpected age recipient type".into()));
    }

    let identity = age::scrypt::Identity::new(passphrase);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| CoreError::Transport(format!("age decrypt: {e}")))?;

    use std::io::Read;
    let mut buf = Zeroizing::new([0u8; 32]);
    reader
        .read_exact(buf.as_mut())
        .map_err(|e| CoreError::Transport(format!("age read: {e}")))?;

    // Guard against larger plaintexts (would indicate corruption or a
    // different-format file at the expected path).
    let mut tail = [0u8; 1];
    if reader.read(&mut tail).unwrap_or(0) > 0 {
        return Err(CoreError::Transport(
            "hs key has unexpected length".into(),
        ));
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_generates_then_loads_same_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();

        let first = load_or_create(&path, &seed).unwrap();
        let second = load_or_create(&path, &seed).unwrap();
        assert_eq!(first.as_ref(), second.as_ref(), "same seed → same key");
    }

    #[test]
    fn different_seed_cannot_decrypt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed_a = Seed::generate().unwrap();
        let seed_b = Seed::generate().unwrap();

        let _ = load_or_create(&path, &seed_a).unwrap();
        let result = load_or_create(&path, &seed_b);
        assert!(
            matches!(result, Err(CoreError::Transport(_))),
            "different seed must fail to decrypt"
        );
    }

    #[test]
    fn fresh_dir_triggers_generate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();
        assert!(!path.exists());
        let _ = load_or_create(&path, &seed).unwrap();
        assert!(path.exists(), "load_or_create must persist on first call");
    }
}
