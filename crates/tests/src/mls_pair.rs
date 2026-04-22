// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test: Alice ↔ Bob MLS 2-member group, exchange messages
//! both ways, survive restart, resume exchange. Runs against in-memory
//! `Pool`s — no Tor, no Noise. 1.E layers MLS over an authenticated
//! Noise channel.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    new_group_repo, new_in_memory_pool, new_kp_repo, new_mls_provider, Group, GroupId,
};

fn env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 0,
        reply_to: None,
        kind: Kind::Text {
            body: body.to_string(),
        },
    }
}

#[test]
fn alice_bob_exchange_messages_and_survive_restart() {
    let psk = [0x5Au8; 32];

    // -- First session --
    let alice_pool = new_in_memory_pool();
    let bob_pool = new_in_memory_pool();
    let bob_kp_repo = new_kp_repo(&bob_pool);
    let alice_group_repo = new_group_repo(&alice_pool);
    let bob_group_repo = new_group_repo(&bob_pool);

    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();

    let bob_provider = new_mls_provider();
    let bob_kp =
        skattr_core::test_exports::KeyPackage::test_generate(&bob_id, &bob_provider, &bob_kp_repo)
            .unwrap();

    let mut alice = Group::test_create_solo(&alice_id, Some(&psk), new_mls_provider()).unwrap();
    let (welcome, _commit) = alice.test_add_member(&bob_kp, Some(&psk)).unwrap();
    let mut bob =
        Group::test_join_from_welcome(&bob_id, &welcome, Some(&psk), bob_provider).unwrap();

    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);
    assert_eq!(alice.id(), bob.id());
    let gid: GroupId = alice.id().clone();

    // Exchange four messages: A→B, B→A, A→B, B→A.
    let m1 = env("hi bob");
    let ct1 = alice.test_encrypt(&m1).unwrap();
    let got1 = bob.test_decrypt(&ct1).unwrap();
    assert_eq!(format!("{got1:?}"), format!("{m1:?}"));

    let m2 = env("hi alice");
    let ct2 = bob.test_encrypt(&m2).unwrap();
    let got2 = alice.test_decrypt(&ct2).unwrap();
    assert_eq!(format!("{got2:?}"), format!("{m2:?}"));

    let m3 = env("how's it going");
    let ct3 = alice.test_encrypt(&m3).unwrap();
    let got3 = bob.test_decrypt(&ct3).unwrap();
    assert_eq!(format!("{got3:?}"), format!("{m3:?}"));

    let m4 = env("all good");
    let ct4 = bob.test_encrypt(&m4).unwrap();
    let got4 = alice.test_decrypt(&ct4).unwrap();
    assert_eq!(format!("{got4:?}"), format!("{m4:?}"));

    // Persist on both sides.
    alice.test_save(&alice_group_repo).unwrap();
    bob.test_save(&bob_group_repo).unwrap();

    // -- Simulated restart: drop both groups, then reload --
    drop(alice);
    drop(bob);

    let mut alice = Group::test_load(&gid, &alice_group_repo)
        .unwrap()
        .expect("alice");
    let mut bob = Group::test_load(&gid, &bob_group_repo)
        .unwrap()
        .expect("bob");
    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);

    // Resume exchange.
    let m5 = env("still here after restart");
    let ct5 = alice.test_encrypt(&m5).unwrap();
    let got5 = bob.test_decrypt(&ct5).unwrap();
    assert_eq!(format!("{got5:?}"), format!("{m5:?}"));

    let m6 = env("bob too");
    let ct6 = bob.test_encrypt(&m6).unwrap();
    let got6 = alice.test_decrypt(&ct6).unwrap();
    assert_eq!(format!("{got6:?}"), format!("{m6:?}"));
}
