// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test: Alice mints invite → Bob parses + verifies +
//! records + marks consumed. No Tor, no Noise, no MLS — just the
//! invite layer exercised against separate in-memory pools.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{InviteLink, KeyPackageRepo, Pool};

#[test]
fn alice_mints_invite_bob_parses_records_and_consumes() {
    let alice_id = IdentityKey::generate().unwrap();
    // Opaque KP bytes — 1.D's invite layer doesn't care what a real MLS
    // KeyPackage looks like. 1.F wires a full end-to-end flow with a
    // real MLS KP from Phase 1.C's KeyPackage::generate.
    let kp_bytes: Vec<u8> = (0..128u8).collect();
    let psk = [0x5A; 32];

    let invite = InviteLink::generate(
        &alice_id,
        "abc.onion".into(),
        kp_bytes.clone(),
        psk,
        3600,
        1_000_000,
    )
    .unwrap();
    let url = invite.to_url().unwrap();

    // Bob's side — separate in-memory pool.
    let bob_pool = Pool::in_memory();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);

    let parsed = InviteLink::from_url(&url, 1_000_010).unwrap();
    assert_eq!(parsed.body.identity, alice_id.public());
    assert_eq!(parsed.body.key_package, kp_bytes);

    parsed.record_received(&bob_kp_repo).unwrap();
    assert!(!parsed.is_consumed(&bob_kp_repo).unwrap());

    parsed.mark_consumed(&bob_kp_repo).unwrap();
    assert!(parsed.is_consumed(&bob_kp_repo).unwrap());

    // Replay attempt — parsing is stateless, so parse still succeeds.
    let reparsed = InviteLink::from_url(&url, 1_000_020).unwrap();
    // record_received is idempotent — no error on second call.
    reparsed.record_received(&bob_kp_repo).unwrap();
    // is_consumed still true — Bob's local state remembers.
    assert!(reparsed.is_consumed(&bob_kp_repo).unwrap());

    // Expiry: past TTL (was 3600 seconds from 1_000_000).
    let err = InviteLink::from_url(&url, 1_003_601).expect_err("expired");
    assert!(
        matches!(
            err,
            skattr_core::error::CoreError::Invite(
                skattr_core::invite::InviteErrorKind::Expired
            )
        ),
        "expected InviteErrorKind::Expired, got {err:?}"
    );
}
