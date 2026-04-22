// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! The single MLS ciphersuite Skattr speaks.
//!
//! `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`
//!
//! Chosen for:
//!
//! - Matching primitives with our Noise transport (ChaCha20-Poly1305,
//!   X25519, SHA-256) — reduces audit surface.
//! - Wide support in OpenMLS.
//! - IANA code-point: 0x0003 (see RFC 9420 §17).
//!
//! Changing this constant is a wire-incompatible change. Upgrades happen
//! via MLS extensions, not by editing this file.

/// MLS ciphersuite IANA code-point.
///
/// RFC 9420 §17: `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` = 0x0003.
/// (0x0001 is `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — a different suite.)
pub const CIPHERSUITE: u16 = 0x0003;
