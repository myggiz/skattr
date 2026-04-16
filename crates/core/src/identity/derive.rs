// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Domain-separated key derivation helpers.
//!
//! Every HKDF use in Skattr passes a distinct `info` string so that
//! derived keys cannot be interchanged across purposes. The canonical
//! labels are listed below; never invent an ad-hoc label in calling code
//! without adding it here first.

use zeroize::Zeroizing;

use crate::error::Result;

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
    let _ = (ikm, info);
    todo!("HKDF-SHA256 extract-then-expand into [u8; OUT]")
}
