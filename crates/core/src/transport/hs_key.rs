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
use crate::transport::TransportErrorKind;

/// Raw bytes of a v3 HS signing key (Ed25519 secret).
pub(crate) type HsSecretBytes = Zeroizing<[u8; 32]>;

/// Fixed scrypt work factor (`N = 2^18`) for at-rest HS-key encryption.
///
/// The age passphrase is a full-entropy 256-bit HKDF-derived key (hex-encoded),
/// so scrypt's low-entropy-passphrase stretching is security-irrelevant here.
/// We pin a fast, deterministic factor instead of age's device-speed
/// auto-calibration (`Encryptor::with_user_passphrase` → `Recipient::new`): a
/// factor tuned to a fast/idle machine at write time gets rejected by age's
/// decrypt-side guard on a slower/loaded machine at read time, which can lock a
/// user out of their own key.
const AGE_WORK_FACTOR: u8 = 18;

/// Ceiling accepted on decrypt (`N = 2^22`). A fixed bound replaces age's
/// per-device default (`target_scrypt_work_factor() + 4`, recomputed each read),
/// so legacy files written with a device-calibrated factor (observed up to 20)
/// are always accepted regardless of the reading machine's speed.
const AGE_MAX_WORK_FACTOR: u8 = 22;

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
    let mut recipient = age::scrypt::Recipient::new(passphrase);
    recipient.set_work_factor(AGE_WORK_FACTOR);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|e| {
                CoreError::Transport(TransportErrorKind::Other(format!("age recipients: {e}")))
            })?;

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("age wrap: {e}"))))?;
    use std::io::Write;
    writer
        .write_all(bytes)
        .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("age write: {e}"))))?;
    writer
        .finish()
        .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("age finish: {e}"))))?;

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
    let decryptor = age::Decryptor::new_buffered(&ciphertext[..]).map_err(|e| {
        CoreError::Transport(TransportErrorKind::Other(format!("age decryptor: {e}")))
    })?;

    if !decryptor.is_scrypt() {
        return Err(CoreError::Transport(TransportErrorKind::Other(
            "unexpected age recipient type".into(),
        )));
    }

    let mut identity = age::scrypt::Identity::new(passphrase);
    identity.set_max_work_factor(AGE_MAX_WORK_FACTOR);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("age decrypt: {e}")))
        })?;

    use std::io::Read;
    let mut buf = Zeroizing::new([0u8; 32]);
    reader
        .read_exact(buf.as_mut())
        .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("age read: {e}"))))?;

    // Guard against larger plaintexts (would indicate corruption or a
    // different-format file at the expected path).
    let mut tail = [0u8; 1];
    if reader.read(&mut tail).unwrap_or(0) > 0 {
        return Err(CoreError::Transport(TransportErrorKind::Other(
            "hs key has unexpected length".into(),
        )));
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
    fn save_pins_fixed_work_factor() {
        // Regression: save() must write a fixed scrypt work factor, not age's
        // device-calibrated one (`with_user_passphrase`). A device-tuned factor
        // written on an idle machine is what the decrypt guard later rejects on
        // a loaded machine. Assert the on-disk age header carries AGE_WORK_FACTOR.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();
        load_or_create(&path, &seed).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let header = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]);
        let line = header
            .lines()
            .find(|l| l.starts_with("-> scrypt"))
            .expect("age scrypt stanza present");
        let log_n: u8 = line
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .expect("work factor parses");
        assert_eq!(
            log_n, AGE_WORK_FACTOR,
            "save must pin a fixed work factor, not device-calibrate"
        );
    }

    #[test]
    fn load_accepts_legacy_high_work_factor_file() {
        // Regression: a key file written with a higher (device-calibrated) work
        // factor than we now pin must still decrypt — this is the exact failure
        // that bricked existing data dirs (files at log_n 19/20). Craft such a
        // file directly and prove load() reads it via the fixed max ceiling.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hs.key.age");
        let seed = Seed::generate().unwrap();

        let key = derive_storage_key(&seed).unwrap();
        let passphrase = age::secrecy::SecretString::from(hex::encode(key.as_ref()));
        let mut recipient = age::scrypt::Recipient::new(passphrase);
        recipient.set_work_factor(20); // highest factor observed on real data dirs
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .unwrap();
        let secret = generate();
        let mut ciphertext = Vec::new();
        {
            use std::io::Write;
            let mut writer = encryptor.wrap_output(&mut ciphertext).unwrap();
            writer.write_all(secret.as_ref()).unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&path, &ciphertext).unwrap();

        let loaded = load(&path, &seed).unwrap();
        assert_eq!(loaded.as_ref(), secret.as_ref());
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
