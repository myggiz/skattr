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

/// Transport↔MLS binding: `HKDF(noise_handshake_hash, "skattr-binding-v1")`.
pub const INFO_TRANSPORT_BINDING_V1: &[u8] = b"skattr-binding-v1";

/// Invite PSK expansion: `HKDF(invite_psk, "skattr-invite-psk-v1")`.
pub const INFO_INVITE_PSK_V1: &[u8] = b"skattr-invite-psk-v1";

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
    fn hkdf_supports_64_byte_output() {
        let ikm = b"ikm";
        let out: [u8; 64] = *hkdf_expand::<64>(ikm, INFO_INVITE_PSK_V1).unwrap();
        // Sanity: first 32 bytes are not equal to last 32 bytes (would imply a bug).
        assert_ne!(&out[..32], &out[32..]);
    }
}
