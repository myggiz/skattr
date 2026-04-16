// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Noise_XK handshake and transport cipher via `snow`.
//!
//! **Pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s`. The responder's
//! static key is assumed known out-of-band (via invite link or cached
//! contact); the initiator's static key is transmitted encrypted.
//!
//! On completion we extract the Noise handshake hash and derive
//! `h_transport = HKDF(hh, "skattr-binding-v1")`, which the MLS layer
//! injects as external PSK into the first Commit. This binding prevents
//! MLS-state replay across different Noise sessions.

use zeroize::Zeroizing;

use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey};

/// Canonical Noise pattern string.
pub const NOISE_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";

/// Outcome of a completed handshake.
pub struct HandshakeOutcome {
    /// Peer's verified Ed25519 identity public key.
    pub peer: PublicKey,
    /// 32-byte binding hash for the MLS layer.
    pub h_transport: Zeroizing<[u8; 32]>,
}

/// Run the initiator side of Noise_XK.
///
/// On success returns the peer identity and the transport binding hash;
/// the underlying stream is wrapped internally and returned alongside
/// by higher layers (see [`super::connection::AuthenticatedConnection`]).
pub async fn handshake_initiator(
    _identity: &IdentityKey,
    _peer_static: &PublicKey,
    _invite_psk: Option<&[u8; 32]>,
) -> Result<HandshakeOutcome> {
    todo!("drive snow HandshakeState as initiator (-e, ee, s, es) with optional PSK")
}

/// Run the responder side of Noise_XK.
pub async fn handshake_responder(
    _identity: &IdentityKey,
    _invite_psk_lookup: impl Fn(&PublicKey) -> Option<[u8; 32]>,
) -> Result<HandshakeOutcome> {
    todo!("drive snow HandshakeState as responder; look up PSK by initiator static key")
}
