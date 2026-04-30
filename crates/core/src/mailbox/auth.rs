// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Wire-frozen helpers shared by `core::mailbox::client` and
//! `crates/mailbox`. Single source of truth for the auth-string
//! construction documented in ADR 0006.

use sha2::{Digest, Sha256};

/// Domain-separation prefix.
pub const AUTH_DOMAIN: &[u8] = b"skattr-mailbox-auth-v1";
/// Operation byte for FETCH (matches `MailboxFrameKind::Fetch`).
pub const OP_BYTE_FETCH: u8 = 0x86;
/// Operation byte for DELETE.
pub const OP_BYTE_DELETE: u8 = 0x88;

/// `sha256(canonical_cbor(payload))`. Returns a `String` error so this
/// helper has zero coupling to either crate's error taxonomy.
pub fn payload_digest<T: serde::Serialize>(payload: &T) -> Result<[u8; 32], String> {
    let mut buf = Vec::new();
    ciborium::into_writer(payload, &mut buf).map_err(|e| format!("auth digest: {e}"))?;
    Ok(Sha256::digest(&buf).into())
}

/// Build the full auth-string input bytes:
/// `AUTH_DOMAIN || nonce || op_byte || payload_digest`.
#[must_use]
pub fn signing_input(nonce: &[u8; 32], op_byte: u8, payload_digest: &[u8; 32]) -> Vec<u8> {
    let mut input = Vec::with_capacity(AUTH_DOMAIN.len() + 32 + 1 + 32);
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(nonce);
    input.push(op_byte);
    input.extend_from_slice(payload_digest);
    input
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn signing_input_layout_is_stable() {
        let nonce = [0x11u8; 32];
        let digest = [0x22u8; 32];
        let out = signing_input(&nonce, OP_BYTE_FETCH, &digest);
        assert!(out.starts_with(AUTH_DOMAIN));
        assert_eq!(&out[AUTH_DOMAIN.len()..AUTH_DOMAIN.len() + 32], &nonce);
        assert_eq!(out[AUTH_DOMAIN.len() + 32], OP_BYTE_FETCH);
        assert_eq!(&out[AUTH_DOMAIN.len() + 32 + 1..], &digest);
    }

    #[test]
    fn payload_digest_produces_32_bytes() {
        let v = (1u16, [9u8; 32], [0xAAu8; 32]);
        let d = payload_digest(&v).unwrap();
        assert_eq!(d.len(), 32);
    }

    #[test]
    fn payload_digest_is_deterministic() {
        let v = (1u16, [9u8; 32], [0xAAu8; 32]);
        let d1 = payload_digest(&v).unwrap();
        let d2 = payload_digest(&v).unwrap();
        assert_eq!(d1, d2);
    }
}
