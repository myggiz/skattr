// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Domain-separated key derivation helpers.
//!
//! Every HKDF use in Skattr passes a distinct `info` string so that
//! derived keys cannot be interchanged across purposes. The canonical
//! labels are listed below; never invent an ad-hoc label in calling code
//! without adding it here first.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};

/// Identity key derivation: `HKDF(seed, "skattr-identity-v1")`.
pub const INFO_IDENTITY_V1: &[u8] = b"skattr-identity-v1";

/// Storage encryption key derivation: `HKDF(seed, "skattr-storage-v1")`.
pub const INFO_STORAGE_V1: &[u8] = b"skattr-storage-v1";

/// HS signing-key at-rest encryption: `HKDF(seed, "skattr-hs-storage-v1")`.
pub const INFO_HS_STORAGE_V1: &[u8] = b"skattr-hs-storage-v1";

/// Transport↔MLS binding: `HKDF(noise_handshake_hash, "skattr-binding-v1")`.
pub const INFO_TRANSPORT_BINDING_V1: &[u8] = b"skattr-binding-v1";

/// Invite PSK expansion: `HKDF(invite_psk, "skattr-invite-psk-v1")`.
pub const INFO_INVITE_PSK_V1: &[u8] = b"skattr-invite-psk-v1";

/// Daemon storage-seed derivation from the identity secret:
/// `HKDF(identity_secret, "skattr-storage-seed-v1")`. The storage seed
/// is used as the base for further at-rest encryption derivations
/// (HS key, SQLite DB) — it is NOT the BIP39 identity seed.
pub const INFO_STORAGE_SEED_V1: &[u8] = b"skattr-storage-seed-v1";

/// Backup archive at-rest encryption:
/// `HKDF(storage_seed, "skattr-backup-v1")`.
pub const INFO_BACKUP_V1: &[u8] = b"skattr-backup-v1";

/// Attachment per-chunk key derivation:
/// `HKDF(file_key, "skattr-attach-v1" || u32_be(chunk_index))` → 56 bytes
/// (32-byte XChaCha20-Poly1305 key ‖ 24-byte XNonce).
pub const INFO_ATTACH_V1: &[u8] = b"skattr-attach-v1";

/// Expand `ikm` into `OUT` bytes of output, bound to `info`.
///
/// Uses HKDF-SHA256 with an empty salt (inputs are already high-entropy).
pub fn hkdf_expand<const OUT: usize>(ikm: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; OUT]>> {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = Zeroizing::new([0u8; OUT]);
    hk.expand(info, okm.as_mut())
        .map_err(|e| CoreError::Identity(format!("hkdf expand: {e}")))?;
    Ok(okm)
}

/// Derive a chunk's 32-byte AEAD key + 24-byte nonce from the manifest
/// `file_key` and the chunk index. Returns 56 bytes: `[0..32]` key,
/// `[32..56]` nonce.
pub fn chunk_key_material(file_key: &[u8; 32], index: u32) -> Result<Zeroizing<[u8; 56]>> {
    let mut info = Vec::with_capacity(INFO_ATTACH_V1.len() + 4);
    info.extend_from_slice(INFO_ATTACH_V1);
    info.extend_from_slice(&index.to_be_bytes());
    hkdf_expand::<56>(file_key, &info)
}

/// Derive the daemon storage seed from an identity key via HKDF.
///
/// Public entry point used by the CLI's daemon handler. Encapsulates
/// access to the identity's raw secret bytes so callers outside the
/// `core` crate don't need direct access.
pub fn derive_storage_seed(
    identity: crate::identity::IdentityKey,
) -> crate::error::Result<crate::identity::Seed> {
    let identity_secret = Zeroizing::new(identity.into_bytes());
    let storage_seed_bytes = hkdf_expand::<32>(identity_secret.as_ref(), INFO_STORAGE_SEED_V1)?;
    Ok(crate::identity::Seed::from_storage_bytes(
        *storage_seed_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_is_deterministic_and_domain_separated() {
        let ikm = b"some input keying material";

        let a: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_IDENTITY_V1).unwrap();
        let b: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_IDENTITY_V1).unwrap();
        assert_eq!(a, b, "HKDF must be deterministic for the same IKM + info");

        let c: [u8; 32] = *hkdf_expand::<32>(ikm, INFO_STORAGE_V1).unwrap();
        assert_ne!(a, c, "different info labels must produce different outputs");
    }

    #[test]
    fn chunk_key_material_is_deterministic_and_per_index() {
        let fk = [0x42u8; 32];
        let a = chunk_key_material(&fk, 0).unwrap();
        let b = chunk_key_material(&fk, 0).unwrap();
        assert_eq!(*a, *b, "same (file_key, index) → same material");
        let c = chunk_key_material(&fk, 1).unwrap();
        assert_ne!(*a, *c, "different index → different material");
        // The key region (0..32) and nonce region (32..56) come from distinct
        // stretches of the HKDF stream: compare equal-length windows by content
        // (the key's first 24 bytes vs the 24-byte nonce) to catch a degenerate
        // repeating derivation.
        assert_ne!(&a[..24], &a[32..56]);
    }

    #[test]
    fn hkdf_supports_64_byte_output() {
        let ikm = b"ikm";
        let out: [u8; 64] = *hkdf_expand::<64>(ikm, INFO_INVITE_PSK_V1).unwrap();
        // Sanity: first 32 bytes are not equal to last 32 bytes (would imply a bug).
        assert_ne!(&out[..32], &out[32..]);
    }
}
