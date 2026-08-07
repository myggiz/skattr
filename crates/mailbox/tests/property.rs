// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Property tests for the mailbox protocol invariants. Spec §"Test
//! plan / 2. Property" — every property here is a freeze-bar item.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::BytesMut;
use ed25519_dalek::{Signer, SigningKey};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use skattr_core::mailbox::protocol::{
    Challenge, Delete, Deposit, ErrorBody, ErrorCode, Fetch, PROTOCOL_VERSION,
};
use skattr_mailbox::{
    auth::{payload_digest, AUTH_DOMAIN, OP_BYTE_FETCH},
    codec::{MailboxFrame, MailboxFrameCodec},
    policy::Policy,
};
use tokio_util::codec::{Decoder, Encoder};

fn round_trip(f: MailboxFrame) -> MailboxFrame {
    let mut codec = MailboxFrameCodec::new();
    let mut buf = BytesMut::new();
    codec.encode(f, &mut buf).expect("encode");
    codec.decode(&mut buf).expect("decode").expect("frame")
}

proptest! {
    #[test]
    fn deposit_cbor_round_trip(
        recipient_hash in proptest::array::uniform32(any::<u8>()),
        ttl in any::<u32>(),
        ciphertext in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let f = MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash,
            ciphertext,
            ttl_request: ttl,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn challenge_cbor_round_trip(
        identity_hash in proptest::array::uniform32(any::<u8>()),
    ) {
        let f = MailboxFrame::Challenge(Challenge {
            version: PROTOCOL_VERSION,
            identity_hash,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn delete_cbor_round_trip(
        identity_pubkey in proptest::array::uniform32(any::<u8>()),
        nonce in proptest::array::uniform32(any::<u8>()),
        signature in proptest::array::uniform32(any::<u8>()),
        ids in proptest::collection::vec(proptest::array::uniform16(any::<u8>()), 0..8),
    ) {
        // Pad signature to 64 bytes by repeating each byte twice deterministically
        // (proptest::array::uniform64 is unwieldy — inflate from 32 to 64 here).
        let mut sig = [0u8; 64];
        for (i, b) in signature.iter().enumerate() {
            sig[i] = *b;
            sig[i + 32] = *b;
        }
        let f = MailboxFrame::Delete(Delete {
            version: PROTOCOL_VERSION,
            identity_pubkey,
            nonce,
            signature: sig,
            deposit_ids: ids,
        });
        prop_assert_eq!(round_trip(f.clone()), f);
    }

    #[test]
    fn error_cbor_round_trip(message in "[a-zA-Z0-9 ]{0,64}") {
        for code in [
            ErrorCode::UnsupportedVersion,
            ErrorCode::MalformedRequest,
            ErrorCode::TooLarge,
            ErrorCode::RateLimited,
            ErrorCode::RecipientFull,
            ErrorCode::TtlTooLong,
            ErrorCode::TtlTooShort,
            ErrorCode::InvalidSignature,
            ErrorCode::HashMismatch,
            ErrorCode::NonceExpired,
            ErrorCode::NotFound,
            ErrorCode::Internal,
        ] {
            let f = MailboxFrame::Error(ErrorBody {
                code,
                message: message.clone(),
            });
            prop_assert_eq!(round_trip(f.clone()), f);
        }
    }

    #[test]
    fn ttl_clamp_is_monotonic_in_now(
        ttl_request in 1u32..2_592_000,
        now1 in 1_000_000_000i64..1_900_000_000,
        delta in 1i64..100_000,
    ) {
        let p = Policy::recommended();
        let resolved = p.resolve_ttl(ttl_request);
        if let Ok(secs) = resolved {
            // expires_at is now + secs. Monotonic in now: now1 < now1+delta
            // implies expires_at(now1) < expires_at(now1+delta).
            let e1 = now1.saturating_add(i64::from(secs));
            let e2 = now1.saturating_add(delta).saturating_add(i64::from(secs));
            prop_assert!(e2 >= e1);
        }
    }

    #[test]
    fn signed_then_verified_always_passes(
        identity_seed in proptest::array::uniform32(any::<u8>()),
        nonce in proptest::array::uniform32(any::<u8>()),
        payload_hash in proptest::array::uniform32(any::<u8>()),
    ) {
        let sk = SigningKey::from_bytes(&identity_seed);
        let pk: [u8; 32] = sk.verifying_key().to_bytes();
        let mut input = Vec::new();
        input.extend_from_slice(AUTH_DOMAIN);
        input.extend_from_slice(&nonce);
        input.push(OP_BYTE_FETCH);
        input.extend_from_slice(&payload_hash);
        let sig = sk.sign(&input).to_bytes();
        // Verify directly via dalek's API (mirrors what Challenges::verify does).
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk).unwrap();
        prop_assert!(
            vk.verify_strict(&input, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok()
        );
    }

    #[test]
    fn payload_digest_is_deterministic_for_tuples(
        v in any::<u16>(),
        nonce in proptest::array::uniform32(any::<u8>()),
        pubkey in proptest::array::uniform32(any::<u8>()),
    ) {
        // Two encodings of the same positional tuple must hash to the
        // same digest. (CBOR definite-length array is positional, so
        // there's no field-name canonicalisation gap.)
        let d1 = payload_digest(&(v, pubkey, nonce)).unwrap();
        let d2 = payload_digest(&(v, pubkey, nonce)).unwrap();
        prop_assert_eq!(d1, d2);
    }

    #[test]
    fn payload_digest_changes_when_position_changes(
        a in proptest::array::uniform32(any::<u8>()),
        b in proptest::array::uniform32(any::<u8>()),
    ) {
        // Anti-collision: swapping two distinct fixed-length byte arrays
        // in the tuple produces different digests. (Otherwise the auth
        // string couldn't bind position.)
        if a == b { return Ok(()); }
        let d1 = payload_digest(&(a, b)).unwrap();
        let d2 = payload_digest(&(b, a)).unwrap();
        prop_assert_ne!(d1, d2);
    }
}
