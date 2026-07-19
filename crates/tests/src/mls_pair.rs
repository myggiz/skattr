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
    Group, GroupId, KeyPackage, KeyPackageRepo, MlsGroupRepo, MlsProvider, Pool,
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

    let alice_pool = Pool::in_memory();
    let bob_pool = Pool::in_memory();
    let bob_kp_repo = KeyPackageRepo::new(&bob_pool);
    let alice_group_repo = MlsGroupRepo::new(&alice_pool);
    let bob_group_repo = MlsGroupRepo::new(&bob_pool);

    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();

    let bob_provider = MlsProvider::new();
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &bob_kp_repo).unwrap();

    // Fixed test KeyPackageRef used identically on both sides to derive the
    // per-invite PSK id (ADR 0009). h_transport binding is None at this layer.
    let kp_ref = [7u8; 32];
    let mut alice =
        Group::create_solo(&alice_id, Some((&kp_ref, &psk)), None, MlsProvider::new()).unwrap();
    let (welcome, _commit) = alice
        .add_member(&bob_kp, Some((&kp_ref, &psk)), None)
        .unwrap();
    let mut bob =
        Group::join_from_welcome(&bob_id, &welcome, Some((&kp_ref, &psk)), None, bob_provider)
            .unwrap();

    // Simulate the Welcome-Ack: the committer (alice) stays PendingJoin until
    // the peer confirms receipt of the Welcome (#93). set_active() mirrors the
    // real Ack path; bob (joiner) is already Active via join_from_welcome.
    alice.set_active();

    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);
    assert_eq!(alice.id(), bob.id());
    let gid: GroupId = alice.id().clone();

    let m1 = env("hi bob");
    let ct1 = alice.encrypt(&m1).unwrap();
    let got1 = bob.decrypt(&ct1).unwrap().expect("app message");
    assert_eq!(format!("{got1:?}"), format!("{m1:?}"));

    let m2 = env("hi alice");
    let ct2 = bob.encrypt(&m2).unwrap();
    let got2 = alice.decrypt(&ct2).unwrap().expect("app message");
    assert_eq!(format!("{got2:?}"), format!("{m2:?}"));

    let m3 = env("how's it going");
    let ct3 = alice.encrypt(&m3).unwrap();
    let got3 = bob.decrypt(&ct3).unwrap().expect("app message");
    assert_eq!(format!("{got3:?}"), format!("{m3:?}"));

    let m4 = env("all good");
    let ct4 = bob.encrypt(&m4).unwrap();
    let got4 = alice.decrypt(&ct4).unwrap().expect("app message");
    assert_eq!(format!("{got4:?}"), format!("{m4:?}"));

    alice.save(&alice_group_repo).unwrap();
    bob.save(&bob_group_repo).unwrap();

    drop(alice);
    drop(bob);

    let mut alice = Group::load(&gid, &alice_group_repo)
        .unwrap()
        .expect("alice");
    let mut bob = Group::load(&gid, &bob_group_repo).unwrap().expect("bob");
    assert_eq!(alice.epoch(), 1);
    assert_eq!(bob.epoch(), 1);

    let m5 = env("still here after restart");
    let ct5 = alice.encrypt(&m5).unwrap();
    let got5 = bob.decrypt(&ct5).unwrap().expect("app message");
    assert_eq!(format!("{got5:?}"), format!("{m5:?}"));

    let m6 = env("bob too");
    let ct6 = bob.encrypt(&m6).unwrap();
    let got6 = alice.decrypt(&ct6).unwrap().expect("app message");
    assert_eq!(format!("{got6:?}"), format!("{m6:?}"));
}
