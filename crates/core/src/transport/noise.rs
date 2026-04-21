// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Noise_XK handshake and transport cipher via `snow`.
//!
//! **Pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s`, optionally with the
//! `psk3` modifier (`Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s`) when an
//! invite PSK is supplied on both sides. The responder's static X25519
//! key is assumed known out-of-band (from a `ContactCard` or invite);
//! the initiator's static key is transmitted encrypted inside msg3.
//!
//! On completion we extract the Noise handshake hash and derive
//! `h_transport = HKDF(hh, "skattr-binding-v1")`, which the MLS layer
//! injects as an external PSK into the first Commit. This binding
//! prevents MLS-state replay across different Noise sessions.
//!
//! The Ed25519 → X25519 bridge (private via SHA-512 clamp, public via
//! the Edwards-Y → Montgomery-U birational map) lives on `IdentityKey`
//! — see `identity::key::{IdentityKey::noise_static_secret,
//! noise_static_public, ed25519_pub_to_x25519}`.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use zeroize::Zeroizing;

use crate::error::Result;
use crate::identity::IdentityKey;
use crate::transport::connection::AuthenticatedConnection;

/// Base Noise pattern string (no PSK modifier).
pub(crate) const NOISE_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";

/// Noise pattern string with a `psk3` modifier — used when the caller
/// supplies an invite PSK on both sides.
pub(crate) const NOISE_PATTERN_PSK3: &str = "Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s";

/// Version byte written by the initiator before the first Noise frame.
/// Responder reads one byte and rejects anything other than this value.
pub(crate) const PROTOCOL_VERSION: u8 = 0x01;

/// Whole-handshake timeout — the three Noise frames plus version
/// preamble must complete inside this window. Defends against slowloris
/// and half-open connections. Surfaces as
/// `CoreError::Transport("handshake: timeout")`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Outcome of a completed handshake.
///
/// The caller (contact layer, not this module) is responsible for
/// mapping `peer_x25519` back to an Ed25519 identity via a ContactCard
/// lookup: iterate known contacts, convert each stored Ed25519 pubkey
/// with `ed25519_pub_to_x25519`, compare. That resolver is outside 1.B.
pub struct HandshakeOutcome {
    /// Peer's X25519 static public key as received during Noise.
    pub peer_x25519: [u8; 32],
    /// 32-byte transport↔MLS binding token:
    /// `HKDF-SHA256(noise_handshake_hash, "skattr-binding-v1")`.
    pub h_transport: Zeroizing<[u8; 32]>,
}

/// Drive the initiator side of Noise_XK over `stream`.
///
/// Writes the 1-byte version preamble, then the -e / -e, ee, s, es /
/// -s, se, (psk) token sequence as three `Frame::NoiseInit` /
/// `Frame::NoiseResp` frames. On success returns an
/// [`AuthenticatedConnection`] wrapping `stream` plus a
/// [`HandshakeOutcome`].
pub async fn handshake_initiator<S>(
    _stream: S,
    _identity: &IdentityKey,
    _peer_static_x25519: &[u8; 32],
    _invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    todo!("drive snow HandshakeState as initiator with optional psk3 + outer timeout")
}

/// Drive the responder side of Noise_XK over `stream`.
///
/// Reads and validates the 1-byte version preamble, then the three
/// Noise frames. On success returns an [`AuthenticatedConnection`]
/// wrapping `stream` plus a [`HandshakeOutcome`]. Identity resolution
/// (X25519 → Ed25519 → ContactCard) is the caller's responsibility.
pub async fn handshake_responder<S>(
    _stream: S,
    _identity: &IdentityKey,
    _invite_psk: Option<&[u8; 32]>,
) -> Result<(AuthenticatedConnection<S>, HandshakeOutcome)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    todo!("drive snow HandshakeState as responder with optional psk3 + outer timeout")
}
