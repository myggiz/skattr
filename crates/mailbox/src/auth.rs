// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Challenge-response auth.
//!
//! Server issues a fresh 32-byte nonce on `Challenge`. Clients sign
//!
//! ```text
//! "skattr-mailbox-auth-v1" || nonce || op_byte || sha256(canonical_cbor(payload_minus_signature))
//! ```
//!
//! with their Ed25519 identity key. The server verifies the
//! signature, the `sha256(pubkey) == identity_hash` binding, and the
//! 30-second nonce TTL. Replay defeated by single-use nonces.

use std::collections::HashMap;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::{AuthErrorKind, MailboxError};

/// Nonce TTL in seconds.
pub const CHALLENGE_TTL_SECS: i64 = 30;

/// Domain-separation prefix for all auth signatures. Bumped if the
/// signing input format ever changes.
pub const AUTH_DOMAIN: &[u8] = b"skattr-mailbox-auth-v1";

/// Operation byte for FETCH (matches `MailboxFrameKind::Fetch`).
pub const OP_BYTE_FETCH: u8 = 0x86;
/// Operation byte for DELETE (matches `MailboxFrameKind::Delete`).
pub const OP_BYTE_DELETE: u8 = 0x88;

#[derive(Debug, Clone, Copy)]
struct Issued {
    identity_hash: [u8; 32],
    issued_at: i64,
}

/// In-memory challenge table. Lock per-server, not per-connection;
/// nonces are short-lived and the row count stays small.
#[derive(Debug, Default)]
pub struct Challenges {
    inner: HashMap<[u8; 32], Issued>,
}

impl Challenges {
    /// Issue a fresh nonce bound to the given identity hash.
    pub fn issue(&mut self, identity_hash: [u8; 32], now: i64) -> [u8; 32] {
        let mut nonce = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut nonce);
        self.inner.insert(
            nonce,
            Issued {
                identity_hash,
                issued_at: now,
            },
        );
        nonce
    }

    /// Verify a signed Fetch/Delete payload. On success, consumes the
    /// nonce so it can't be replayed. The `op_byte` argument is one of
    /// `OP_BYTE_FETCH` or `OP_BYTE_DELETE`. `payload_hash` is
    /// `sha256(canonical_cbor(payload_minus_signature))` — the dispatch
    /// handler computes this once before calling.
    pub fn verify(
        &mut self,
        nonce: [u8; 32],
        identity_pubkey: [u8; 32],
        signature: &[u8; 64],
        op_byte: u8,
        payload_hash: [u8; 32],
        now: i64,
    ) -> Result<(), MailboxError> {
        let issued = self
            .inner
            .get(&nonce)
            .copied()
            .ok_or(MailboxError::Auth(AuthErrorKind::NonceExpired))?;
        if now - issued.issued_at > CHALLENGE_TTL_SECS {
            self.inner.remove(&nonce);
            return Err(MailboxError::Auth(AuthErrorKind::NonceExpired));
        }
        let computed_hash: [u8; 32] = Sha256::digest(identity_pubkey).into();
        if computed_hash != issued.identity_hash {
            return Err(MailboxError::Auth(AuthErrorKind::HashMismatch));
        }

        let mut signing_input = Vec::with_capacity(AUTH_DOMAIN.len() + 32 + 1 + 32);
        signing_input.extend_from_slice(AUTH_DOMAIN);
        signing_input.extend_from_slice(&nonce);
        signing_input.push(op_byte);
        signing_input.extend_from_slice(&payload_hash);

        let vk = VerifyingKey::from_bytes(&identity_pubkey)
            .map_err(|_| MailboxError::Auth(AuthErrorKind::InvalidSignature))?;
        let sig = Signature::from_bytes(signature);
        vk.verify(&signing_input, &sig)
            .map_err(|_| MailboxError::Auth(AuthErrorKind::InvalidSignature))?;

        // Single-use: consume on successful verify.
        self.inner.remove(&nonce);
        Ok(())
    }

    /// Drop nonces past their TTL. Called periodically by the server.
    /// Returns the number evicted.
    pub fn sweep(&mut self, now: i64) -> u64 {
        let stale: Vec<[u8; 32]> = self
            .inner
            .iter()
            .filter(|(_, v)| now - v.issued_at > CHALLENGE_TTL_SECS)
            .map(|(k, _)| *k)
            .collect();
        let n = stale.len() as u64;
        for k in stale {
            self.inner.remove(&k);
        }
        n
    }

    /// Number of currently-tracked nonces. For tests and metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` if there are no tracked nonces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Compute `sha256(canonical_cbor(payload))` — the helper used by
/// dispatch handlers to build the signing input over the
/// payload-minus-signature.
pub fn payload_digest<T: serde::Serialize>(payload: &T) -> Result<[u8; 32], MailboxError> {
    let mut buf = Vec::new();
    ciborium::into_writer(payload, &mut buf).map_err(|e| {
        MailboxError::Transport(crate::error::TransportErrorKind::EncodeFailed(format!(
            "auth digest: {e}"
        )))
    })?;
    Ok(Sha256::digest(&buf).into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn signed_input(nonce: [u8; 32], op: u8, payload_hash: [u8; 32], sk: &SigningKey) -> [u8; 64] {
        let mut input = Vec::new();
        input.extend_from_slice(AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(op);
        input.extend_from_slice(&payload_hash);
        sk.sign(&input).to_bytes()
    }

    #[test]
    fn verify_happy_path_consumes_nonce() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();

        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xAB; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);

        c.verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .unwrap();
        assert!(c.is_empty(), "nonce must be consumed");
    }

    #[test]
    fn replay_after_consume_fails_nonce_expired() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xCD; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        c.verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .unwrap();
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .expect_err("must reject replay");
        assert!(matches!(
            err,
            MailboxError::Auth(AuthErrorKind::NonceExpired)
        ));
    }

    #[test]
    fn nonce_expires_after_ttl() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xEE; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        let err = c
            .verify(
                nonce,
                pk,
                &sig,
                OP_BYTE_FETCH,
                payload_hash,
                100 + CHALLENGE_TTL_SECS + 1,
            )
            .expect_err("must reject expired");
        assert!(matches!(
            err,
            MailboxError::Auth(AuthErrorKind::NonceExpired)
        ));
    }

    #[test]
    fn hash_mismatch_rejected() {
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let mut c = Challenges::default();
        let nonce = c.issue([0xFF; 32], 100); // wrong identity hash bound to nonce
        let sig = signed_input(nonce, OP_BYTE_FETCH, [0; 32], &sk);
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_FETCH, [0; 32], 110)
            .expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Auth(AuthErrorKind::HashMismatch)
        ));
    }

    #[test]
    fn signed_by_wrong_key_rejected() {
        let real = SigningKey::generate(&mut OsRng);
        let attacker = SigningKey::generate(&mut OsRng);
        let real_pk: [u8; 32] = real.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(real_pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0xAA; 32];
        // Sign with attacker's key, present real's pubkey.
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &attacker);
        let err = c
            .verify(nonce, real_pk, &sig, OP_BYTE_FETCH, payload_hash, 110)
            .expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Auth(AuthErrorKind::InvalidSignature)
        ));
    }

    #[test]
    fn cross_op_signature_rejected() {
        // Sign for FETCH op_byte, present as DELETE — must not verify.
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = Sha256::digest(pk).into();
        let mut c = Challenges::default();
        let nonce = c.issue(id_hash, 100);
        let payload_hash = [0x42; 32];
        let sig = signed_input(nonce, OP_BYTE_FETCH, payload_hash, &sk);
        let err = c
            .verify(nonce, pk, &sig, OP_BYTE_DELETE, payload_hash, 110)
            .expect_err("must reject cross-op");
        assert!(matches!(
            err,
            MailboxError::Auth(AuthErrorKind::InvalidSignature)
        ));
    }

    #[test]
    fn sweep_evicts_only_expired() {
        let mut c = Challenges::default();
        c.issue([0; 32], 100);
        c.issue([1; 32], 200);
        let evicted = c.sweep(100 + CHALLENGE_TTL_SECS + 1);
        assert_eq!(evicted, 1);
        assert_eq!(c.len(), 1);
    }
}
