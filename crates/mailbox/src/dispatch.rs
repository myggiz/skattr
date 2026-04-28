// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Pure per-frame request handlers.
//!
//! Each `handle_*` returns a [`MailboxFrame`] for the server to send
//! back. Errors are projected to [`ErrorBody`] frames via
//! [`MailboxError::to_wire_code`] in the caller; handlers themselves
//! don't decide whether to close the connection (the FSM in
//! `server.rs` always keeps the stream open after a typed error).
//!
//! Mutex poisoning on the shared `Challenges` / `ConnRateLimiter` is
//! propagated as a typed error ([`AuthErrorKind::Poisoned`] /
//! [`PolicyErrorKind::Poisoned`]) which the wire boundary folds to
//! [`ErrorCode::Internal`]. This mirrors the pattern from
//! `crates/core/src/storage/pool.rs`.

use std::sync::Mutex;

use sha2::Digest as _;
use skattr_core::mailbox::protocol::{
    Challenge, ChallengeNonce, Delete, DeleteOk, Deposit, DepositOk, ErrorBody, Fetch,
    FetchResponse, PendingDeposit, PROTOCOL_VERSION,
};

use crate::auth::{payload_digest, Challenges, OP_BYTE_DELETE, OP_BYTE_FETCH};
use crate::codec::MailboxFrame;
use crate::error::{AuthErrorKind, MailboxError, PolicyErrorKind, TransportErrorKind};
use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
use crate::store::Store;

/// Cluster of references the handlers need. Keeping this as one
/// struct shortens the per-handler signatures.
pub struct DispatchCtx<'a> {
    /// Backing deposit store.
    pub store: &'a Store,
    /// Operator policy (size, TTL, caps).
    pub policy: &'a Policy,
    /// Shared challenge-nonce table (issued + verified here).
    pub challenges: &'a Mutex<Challenges>,
    /// Per-connection token-bucket pair (deposit + fetch).
    pub conn_rl: &'a Mutex<ConnRateLimiter>,
    /// Server-wide deposit token bucket.
    pub global_rl: &'a GlobalRateLimiter,
}

fn check_version(version: u16) -> Result<(), MailboxError> {
    if version != PROTOCOL_VERSION {
        return Err(MailboxError::Transport(
            TransportErrorKind::UnsupportedVersion,
        ));
    }
    Ok(())
}

/// Build the wire `ErrorBody` for any `MailboxError`. Used by the
/// dispatcher when it needs to convert an internal error into the
/// frame to write back.
#[must_use]
pub fn error_frame(err: &MailboxError) -> MailboxFrame {
    let code = err.to_wire_code();
    // Never include payload-derived strings in `message`; only stable
    // human prose. Drives the redaction unit test in Task 24.
    let message = match err {
        MailboxError::Auth(_) => "auth failed".to_string(),
        MailboxError::Policy(PolicyErrorKind::TooLarge) => "too large".to_string(),
        MailboxError::Policy(PolicyErrorKind::TtlTooLong) => "ttl too long".to_string(),
        MailboxError::Policy(PolicyErrorKind::TtlTooShort) => "ttl too short".to_string(),
        MailboxError::Policy(PolicyErrorKind::RateLimited) => "rate limited".to_string(),
        MailboxError::Policy(PolicyErrorKind::RecipientFull) => "recipient full".to_string(),
        MailboxError::Transport(TransportErrorKind::UnsupportedVersion) => {
            "unsupported version".to_string()
        }
        MailboxError::Transport(TransportErrorKind::DecodeFailed(_)) => {
            "malformed request".to_string()
        }
        _ => "internal error".to_string(),
    };
    MailboxFrame::Error(ErrorBody { code, message })
}

/// `Deposit` handler.
pub fn handle_deposit(
    ctx: &DispatchCtx<'_>,
    body: Deposit,
    now: i64,
    now_secs_f: f64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;
    if u64::try_from(body.ciphertext.len()).unwrap_or(u64::MAX) > ctx.policy.max_deposit_size {
        return Err(MailboxError::Policy(PolicyErrorKind::TooLarge));
    }

    {
        let mut rl = ctx
            .conn_rl
            .lock()
            .map_err(|_| MailboxError::Policy(PolicyErrorKind::Poisoned))?;
        rl.deposits.try_acquire(now_secs_f)?;
    }
    ctx.global_rl.try_acquire(now_secs_f)?;

    let ttl = ctx.policy.resolve_ttl(body.ttl_request)?;
    let expires_at = now.saturating_add(i64::from(ttl));

    let id = ctx.store.insert(
        body.recipient_hash,
        body.ciphertext,
        now,
        expires_at,
        ctx.policy.recipient_cap_bytes,
        now,
    )?;

    Ok(MailboxFrame::DepositOk(DepositOk {
        deposit_id: id,
        expires_at,
    }))
}

/// `Challenge` handler.
pub fn handle_challenge(
    ctx: &DispatchCtx<'_>,
    body: Challenge,
    now: i64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;
    let mut c = ctx
        .challenges
        .lock()
        .map_err(|_| MailboxError::Auth(AuthErrorKind::Poisoned))?;
    let nonce = c.issue(body.identity_hash, now);
    Ok(MailboxFrame::ChallengeNonce(ChallengeNonce {
        nonce,
        issued_at: now,
    }))
}

/// `Fetch` handler.
pub fn handle_fetch(
    ctx: &DispatchCtx<'_>,
    body: Fetch,
    now: i64,
    now_secs_f: f64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;

    {
        let mut rl = ctx
            .conn_rl
            .lock()
            .map_err(|_| MailboxError::Policy(PolicyErrorKind::Poisoned))?;
        rl.fetches.try_acquire(now_secs_f)?;
    }

    // Build the canonical payload-minus-signature for the digest.
    #[derive(serde::Serialize)]
    struct Signed<'a> {
        version: u16,
        identity_pubkey: &'a [u8; 32],
        nonce: &'a [u8; 32],
    }
    let digest = payload_digest(&Signed {
        version: body.version,
        identity_pubkey: &body.identity_pubkey,
        nonce: &body.nonce,
    })?;

    {
        let mut c = ctx
            .challenges
            .lock()
            .map_err(|_| MailboxError::Auth(AuthErrorKind::Poisoned))?;
        c.verify(
            body.nonce,
            body.identity_pubkey,
            &body.signature,
            OP_BYTE_FETCH,
            digest,
            now,
        )?;
    }

    let recipient_hash: [u8; 32] = sha2::Sha256::digest(body.identity_pubkey).into();
    let stored = ctx.store.fetch(recipient_hash, now)?;
    let deposits = stored
        .into_iter()
        .map(|s| PendingDeposit {
            deposit_id: s.deposit_id,
            ciphertext: s.ciphertext,
            received_at: s.received_at,
        })
        .collect();
    Ok(MailboxFrame::FetchResponse(FetchResponse { deposits }))
}

/// `Delete` handler.
pub fn handle_delete(
    ctx: &DispatchCtx<'_>,
    body: Delete,
    now: i64,
) -> Result<MailboxFrame, MailboxError> {
    check_version(body.version)?;

    #[derive(serde::Serialize)]
    struct Signed<'a> {
        version: u16,
        identity_pubkey: &'a [u8; 32],
        nonce: &'a [u8; 32],
        deposit_ids: &'a [[u8; 16]],
    }
    let digest = payload_digest(&Signed {
        version: body.version,
        identity_pubkey: &body.identity_pubkey,
        nonce: &body.nonce,
        deposit_ids: &body.deposit_ids,
    })?;

    {
        let mut c = ctx
            .challenges
            .lock()
            .map_err(|_| MailboxError::Auth(AuthErrorKind::Poisoned))?;
        c.verify(
            body.nonce,
            body.identity_pubkey,
            &body.signature,
            OP_BYTE_DELETE,
            digest,
            now,
        )?;
    }

    let recipient_hash: [u8; 32] = sha2::Sha256::digest(body.identity_pubkey).into();
    let (deleted, not_found) = ctx.store.delete(recipient_hash, &body.deposit_ids)?;
    Ok(MailboxFrame::DeleteOk(DeleteOk { deleted, not_found }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use sha2::Digest;
    use skattr_core::mailbox::protocol::ErrorCode;

    use crate::auth::Challenges;
    use crate::codec::MailboxFrame;
    use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
    use crate::store::Store;

    fn fixture() -> (
        Store,
        Policy,
        Mutex<Challenges>,
        Mutex<ConnRateLimiter>,
        GlobalRateLimiter,
    ) {
        let store = Store::in_memory().unwrap();
        let policy = Policy::recommended();
        let chal = Mutex::new(Challenges::default());
        let conn = Mutex::new(ConnRateLimiter::from_policy(&policy, 0.0));
        let global = GlobalRateLimiter::from_policy(&policy, 0.0);
        (store, policy, chal, conn, global)
    }

    #[test]
    fn deposit_happy_path() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_deposit(
            &ctx,
            Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0xAA; 32],
                ciphertext: vec![1, 2, 3, 4],
                ttl_request: 86_400,
            },
            100,
            0.0,
        )
        .unwrap();
        assert!(matches!(resp, MailboxFrame::DepositOk(_)));
    }

    #[test]
    fn deposit_too_large() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_deposit(
            &ctx,
            Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0; 32],
                ciphertext: vec![0; (policy.max_deposit_size + 1) as usize],
                ttl_request: 86_400,
            },
            100,
            0.0,
        )
        .expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::TooLarge);
    }

    #[test]
    fn deposit_unsupported_version() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_deposit(
            &ctx,
            Deposit {
                version: 99,
                recipient_hash: [0; 32],
                ciphertext: vec![],
                ttl_request: 0,
            },
            100,
            0.0,
        )
        .expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::UnsupportedVersion);
    }

    #[test]
    fn challenge_returns_nonce() {
        let (store, policy, chal, conn, global) = fixture();
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_challenge(
            &ctx,
            Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            },
            100,
        )
        .unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));
    }

    fn build_signed_fetch(sk: &SigningKey, nonce: [u8; 32]) -> Fetch {
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        // Compute digest the same way handle_fetch does.
        #[derive(serde::Serialize)]
        struct Signed<'a> {
            version: u16,
            identity_pubkey: &'a [u8; 32],
            nonce: &'a [u8; 32],
        }
        let digest = payload_digest(&Signed {
            version: PROTOCOL_VERSION,
            identity_pubkey: &pk,
            nonce: &nonce,
        })
        .unwrap();
        let mut input = Vec::new();
        input.extend_from_slice(crate::auth::AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(OP_BYTE_FETCH);
        input.extend_from_slice(&digest);
        let sig = sk.sign(&input).to_bytes();
        Fetch {
            version: PROTOCOL_VERSION,
            identity_pubkey: pk,
            nonce,
            signature: sig,
        }
    }

    #[test]
    fn fetch_happy_path_returns_pending_deposits() {
        let (store, policy, chal, conn, global) = fixture();
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = sha2::Sha256::digest(pk).into();
        // Pre-deposit something for the recipient.
        store
            .insert(
                id_hash,
                vec![9, 9, 9],
                100,
                999_999,
                policy.recipient_cap_bytes,
                100,
            )
            .unwrap();
        // Issue a nonce.
        let nonce = chal.lock().unwrap().issue(id_hash, 200);
        let fetch = build_signed_fetch(&sk, nonce);
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let resp = handle_fetch(&ctx, fetch, 210, 0.0).unwrap();
        if let MailboxFrame::FetchResponse(r) = resp {
            assert_eq!(r.deposits.len(), 1);
        } else {
            panic!("expected FetchResponse");
        }
    }

    #[test]
    fn fetch_invalid_signature_rejected() {
        let (store, policy, chal, conn, global) = fixture();
        let sk = SigningKey::generate(&mut OsRng);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let id_hash: [u8; 32] = sha2::Sha256::digest(pk).into();
        let nonce = chal.lock().unwrap().issue(id_hash, 200);
        let mut fetch = build_signed_fetch(&sk, nonce);
        fetch.signature[0] ^= 0xFF;
        let ctx = DispatchCtx {
            store: &store,
            policy: &policy,
            challenges: &chal,
            conn_rl: &conn,
            global_rl: &global,
        };
        let err = handle_fetch(&ctx, fetch, 210, 0.0).expect_err("must reject");
        assert_eq!(err.to_wire_code(), ErrorCode::InvalidSignature);
    }
}
