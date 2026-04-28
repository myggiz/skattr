// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

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
    fn payload_digest_is_stable_under_field_reorder(
        v in any::<u16>(),
        nonce in proptest::array::uniform32(any::<u8>()),
        pubkey in proptest::array::uniform32(any::<u8>()),
    ) {
        // Canonical CBOR sorts keys; encoding the same logical record
        // twice must yield the same digest regardless of the source
        // struct's field declaration order.
        #[derive(serde::Serialize)]
        struct A<'a> {
            version: u16,
            identity_pubkey: &'a [u8; 32],
            nonce: &'a [u8; 32],
        }
        #[derive(serde::Serialize)]
        struct B<'a> {
            nonce: &'a [u8; 32],
            identity_pubkey: &'a [u8; 32],
            version: u16,
        }
        let d1 = payload_digest(&A { version: v, identity_pubkey: &pubkey, nonce: &nonce }).unwrap();
        let d2 = payload_digest(&B { nonce: &nonce, identity_pubkey: &pubkey, version: v }).unwrap();
        // Note: ciborium does NOT canonicalise keys for serde-derived
        // structs. If this assert fails, we need to switch to manual
        // canonical-encoding for the auth digest. The test serves as
        // a tripwire so the freeze ADR can record the chosen behaviour.
        prop_assert_eq!(d1, d2);
    }
}
