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

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit, Payload};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};
use crate::identity::IdentityKey;

/// On-disk vault format version. Bumped only via an ADR.
pub const VAULT_VERSION: u8 = 1;

/// AEAD associated-data binding the ciphertext to this exact format version.
const VAULT_AAD: &[u8] = b"skattr-vault-v1";

/// Run Argon2id on `passphrase` with `salt` and `kdf` params, producing a
/// 32-byte AEAD key.
///
/// The returned buffer zeros on drop; callers must not stash the raw bytes.
///
/// **Passphrase bytes are used verbatim** — no Unicode normalization is
/// applied. ASCII passphrases are recommended; non-ASCII entries will not
/// round-trip across OSes with different default Unicode forms. See
/// `docs/adr/0004-passphrase-normalization.md`.
fn derive_aead_key(
    passphrase: &str,
    salt: &[u8; 16],
    kdf: &KdfParams,
) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32))
        .map_err(|e| CoreError::Identity(format!("argon2 params: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, out.as_mut())
        .map_err(|e| CoreError::Identity(format!("argon2 hash: {e}")))?;
    Ok(out)
}

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
        Self {
            m_kib: 64 * 1024,
            t: 3,
            p: 4,
        }
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

/// Durably write `vf` to `path`: serialize → tempfile → fsync tempfile →
/// rename over target → fsync parent directory.
///
/// Renames on POSIX are atomic within a single filesystem, but durability
/// against power loss additionally requires fsync on both the tempfile
/// and the parent directory (so the directory entry's inode change is
/// on platter before we report success).
fn atomic_write_vault(path: &Path, vf: &VaultFile) -> Result<()> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(vf, &mut buf).map_err(|e| CoreError::CborEncode(e.to_string()))?;

    let tmp_path = path.with_extension("vault.tmp");
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        use std::io::Write;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;

    // Fsync parent directory so the rename itself is durable.
    if let Some(parent) = path.parent() {
        // On Linux the directory must be opened read-only; on macOS
        // File::open works too. Windows has no directory fsync — skip.
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = parent; // suppress unused on non-unix
        }
    }
    Ok(())
}

/// On-disk encrypted identity container.
#[derive(Debug)]
pub struct Vault {
    // Path we opened; used by change_passphrase to rewrite atomically.
    path: std::path::PathBuf,
}

impl Vault {
    /// Create a new vault at `path`, encrypting `identity` under `passphrase`.
    ///
    /// Fails if the file already exists — callers must delete the old
    /// vault first (explicit user intent).
    pub fn create(path: &Path, identity: IdentityKey, passphrase: &str) -> Result<Self> {
        if path.exists() {
            return Err(CoreError::Identity(format!(
                "vault already exists at {}",
                path.display()
            )));
        }

        let kdf = KdfParams::canonical();
        let mut salt = [0u8; 16];
        let mut nonce_bytes = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

        let aead_key = derive_aead_key(passphrase, &salt, &kdf)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
        let nonce = XNonce::from_slice(&nonce_bytes);

        // `Zeroizing<[u8; 32]>` ensures the plaintext secret is wiped when this
        // binding drops, even on an early-return error path.
        let secret_bytes = Zeroizing::new(identity.into_bytes());
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: secret_bytes.as_ref(),
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| CoreError::Identity("aead encrypt failed".into()))?;

        let vf = VaultFile {
            v: VAULT_VERSION,
            kdf,
            salt,
            nonce: nonce_bytes,
            ciphertext,
        };

        atomic_write_vault(path, &vf)?;

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Open an existing vault, decrypting with `passphrase`.
    pub fn open(path: &Path, passphrase: &str) -> Result<(Self, IdentityKey)> {
        let bytes = std::fs::read(path)?;
        let vf: VaultFile = ciborium::de::from_reader(&bytes[..])
            .map_err(|e| CoreError::CborDecode(e.to_string()))?;

        if vf.v != VAULT_VERSION {
            return Err(CoreError::Identity(format!(
                "unsupported vault version {} (expected {VAULT_VERSION})",
                vf.v
            )));
        }

        let aead_key = derive_aead_key(passphrase, &vf.salt, &vf.kdf)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
        let nonce = XNonce::from_slice(&vf.nonce);

        // Decrypt in-place directly into a Zeroizing<[u8; 32]>. The wire
        // format is `ct_body (32 bytes) || poly1305_tag (16 bytes)`; we
        // split them explicitly so AEAD output never touches a Vec<u8>.
        const POLY1305_TAG_LEN: usize = 16;
        const PLAINTEXT_LEN: usize = 32;
        if vf.ciphertext.len() != PLAINTEXT_LEN + POLY1305_TAG_LEN {
            return Err(CoreError::Identity(
                "ciphertext has unexpected length".into(),
            ));
        }
        let (ct_body, tag_bytes) = vf.ciphertext.split_at(PLAINTEXT_LEN);
        let tag = Tag::from_slice(tag_bytes);

        let mut secret = Zeroizing::new([0u8; 32]);
        secret.copy_from_slice(ct_body);
        cipher
            .decrypt_in_place_detached(nonce, VAULT_AAD, secret.as_mut(), tag)
            .map_err(|_| CoreError::Identity("verification failed".into()))?;

        Ok((
            Self {
                path: path.to_path_buf(),
            },
            IdentityKey::from_bytes(secret),
        ))
    }

    /// Re-encrypt the vault under a new passphrase.
    ///
    /// Crash-safe: writes the new vault to a sidecar, fsyncs, then
    /// renames over the existing path atomically. A crash at any point
    /// either leaves the old vault intact (rename hasn't landed) or the
    /// new one (rename has landed) — never neither.
    ///
    /// Takes `&mut self` to serialize concurrent rewrites at the API
    /// boundary — no `self` field is actually mutated; the mutation is
    /// on-disk.
    pub fn change_passphrase(&mut self, old: &str, new: &str) -> Result<()> {
        // Decrypt with the old passphrase first; if it fails, don't touch
        // the file.
        let (_, identity) = Vault::open(&self.path, old)?;

        // Rebuild a fresh VaultFile under the new passphrase, then write
        // atomically over the existing path. Fresh salt + nonce per
        // rewrite fall out of this flow.
        let kdf = KdfParams::canonical();
        let mut salt = [0u8; 16];
        let mut nonce_bytes = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

        let aead_key = derive_aead_key(new, &salt, &kdf)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
        let nonce = XNonce::from_slice(&nonce_bytes);

        let secret_bytes = Zeroizing::new(identity.into_bytes());
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: secret_bytes.as_ref(),
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| CoreError::Identity("aead encrypt failed".into()))?;

        let vf = VaultFile {
            v: VAULT_VERSION,
            kdf,
            salt,
            nonce: nonce_bytes,
            ciphertext,
        };

        atomic_write_vault(&self.path, &vf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_file_cbor_roundtrips() {
        let v = VaultFile {
            v: VAULT_VERSION,
            kdf: KdfParams {
                m_kib: 65536,
                t: 3,
                p: 4,
            },
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

    #[test]
    fn argon2_derive_is_deterministic() {
        let salt = [0x11; 16];
        let kdf = KdfParams::canonical();
        let a = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
        let b = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn argon2_derive_is_passphrase_sensitive() {
        let salt = [0x22; 16];
        let kdf = KdfParams::canonical();
        let a = derive_aead_key("correct horse battery staple", &salt, &kdf).unwrap();
        let b = derive_aead_key("incorrect horse battery staple", &salt, &kdf).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn create_writes_a_valid_cbor_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("identity.vault");
        let id = IdentityKey::generate().unwrap();
        let _vault = Vault::create(&path, id, "hunter2").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _parsed: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
    }

    #[test]
    fn create_refuses_existing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exists.vault");
        std::fs::write(&path, b"placeholder").unwrap();
        let id = IdentityKey::generate().unwrap();
        let err = Vault::create(&path, id, "pw").expect_err("must refuse to overwrite");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn open_recovers_the_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        let expected = id.public();
        Vault::create(&path, id, "pw").unwrap();
        let (_vault, opened) = Vault::open(&path, "pw").unwrap();
        assert_eq!(opened.public(), expected);
    }

    #[test]
    fn open_rejects_wrong_passphrase() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "correct").unwrap();
        let err = Vault::open(&path, "wrong").expect_err("wrong passphrase must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn open_rejects_wrong_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();

        // Manually rewrite the file with v = 99.
        let bytes = std::fs::read(&path).unwrap();
        let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
        vf.v = 99;
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&vf, &mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = Vault::open(&path, "pw").expect_err("unknown version must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn any_ciphertext_bitflip_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();

        // Flip the first ciphertext bit.
        vf.ciphertext[0] ^= 0x01;

        let mut buf = Vec::new();
        ciborium::ser::into_writer(&vf, &mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = Vault::open(&path, "pw").expect_err("bit-flip must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn aad_mismatch_is_detected() {
        // Synthesize a vault whose ciphertext was encrypted under a different AAD
        // and verify it fails open. Easiest: build the VaultFile directly with an
        // AEAD-encrypted blob that used the wrong AAD.
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

        let kdf = KdfParams::canonical();
        let salt = [0xAA; 16];
        let nonce_bytes = [0xBB; 24];
        let aead_key = super::derive_aead_key("pw", &salt, &kdf).unwrap();
        let cipher = XChaCha20Poly1305::new(Key::from_slice(aead_key.as_ref()));
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &[0u8; 32],
                    aad: b"different-aad",
                },
            )
            .unwrap();

        let vf = VaultFile {
            v: VAULT_VERSION,
            kdf,
            salt,
            nonce: nonce_bytes,
            ciphertext,
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad_aad.vault");
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&vf, &mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = Vault::open(&path, "pw").expect_err("AAD mismatch must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn change_passphrase_rotates_salt_and_nonce() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        let expected_pub = id.public();
        Vault::create(&path, id, "old-pw").unwrap();

        let before = std::fs::read(&path).unwrap();
        let before_vf: VaultFile = ciborium::de::from_reader(&before[..]).unwrap();

        let (mut vault, _) = Vault::open(&path, "old-pw").unwrap();
        vault.change_passphrase("old-pw", "new-pw").unwrap();

        let after = std::fs::read(&path).unwrap();
        let after_vf: VaultFile = ciborium::de::from_reader(&after[..]).unwrap();

        assert_ne!(before_vf.salt, after_vf.salt, "salt must rotate");
        assert_ne!(before_vf.nonce, after_vf.nonce, "nonce must rotate");

        // Old passphrase no longer works.
        Vault::open(&path, "old-pw").expect_err("old passphrase must fail");
        // New passphrase recovers the same identity.
        let (_, opened) = Vault::open(&path, "new-pw").unwrap();
        assert_eq!(opened.public(), expected_pub);
    }

    #[test]
    fn change_passphrase_rejects_wrong_old() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "real").unwrap();
        let (mut vault, _) = Vault::open(&path, "real").unwrap();
        let err = vault
            .change_passphrase("bogus", "whatever")
            .expect_err("must reject wrong old passphrase");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
        // File untouched: old passphrase still works.
        Vault::open(&path, "real").unwrap();
    }

    #[test]
    fn argon2_derive_is_salt_sensitive() {
        let kdf = KdfParams::canonical();
        let a = derive_aead_key("pw", &[0x11; 16], &kdf).unwrap();
        let b = derive_aead_key("pw", &[0x22; 16], &kdf).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn argon2_derive_is_params_sensitive() {
        let salt = [0x33; 16];
        let a = derive_aead_key("pw", &salt, &KdfParams::canonical()).unwrap();
        let b = derive_aead_key(
            "pw",
            &salt,
            &KdfParams {
                m_kib: 64 * 1024,
                t: 4, // vs canonical 3
                p: 4,
            },
        )
        .unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn no_tempfile_sidecar_after_create() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();
        let sidecar = path.with_extension("vault.tmp");
        assert!(
            !sidecar.exists(),
            "tempfile sidecar must be gone after create"
        );
    }

    #[test]
    fn open_rejects_wrong_length_ciphertext() {
        // Synthesize a vault whose ciphertext is the wrong length. Must
        // be rejected BEFORE attempting AEAD decrypt.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad_len.vault");
        let id = IdentityKey::generate().unwrap();
        Vault::create(&path, id, "pw").unwrap();

        // Mutate the CBOR: strip two ciphertext bytes.
        let bytes = std::fs::read(&path).unwrap();
        let mut vf: VaultFile = ciborium::de::from_reader(&bytes[..]).unwrap();
        vf.ciphertext.truncate(vf.ciphertext.len() - 2);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&vf, &mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = Vault::open(&path, "pw")
            .expect_err("truncated ciphertext must fail");
        assert!(matches!(err, crate::error::CoreError::Identity(_)));
    }

    #[test]
    fn change_passphrase_survives_simulated_new_create_failure() {
        // Simulate the "disk full during new-vault write" failure by pre-creating
        // a busy/unwritable sidecar path before change_passphrase. The rename
        // should fail cleanly and the OLD vault must still be openable.
        //
        // We approximate "unwritable sidecar" by pre-creating `.vault.tmp` as
        // a directory — File::create will fail with IsADirectory on Unix.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("id.vault");
        let id = IdentityKey::generate().unwrap();
        let expected_pub = id.public();
        Vault::create(&path, id, "old").unwrap();

        // Block the sidecar path.
        std::fs::create_dir(path.with_extension("vault.tmp")).unwrap();

        let (mut vault, _) = Vault::open(&path, "old").unwrap();
        let err = vault.change_passphrase("old", "new");
        assert!(err.is_err(), "sidecar conflict must return Err");

        // Unblock and verify the old vault is still intact.
        std::fs::remove_dir(path.with_extension("vault.tmp")).unwrap();
        let (_, opened) = Vault::open(&path, "old").unwrap();
        assert_eq!(opened.public(), expected_pub);
    }
}
