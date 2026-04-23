// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 0.D integration test: Pool + repos survive a close/reopen cycle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use skattr_core::contact::Contact;
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::{PublicKey, Seed};

// Pool + repos are pub(crate); the test-harness feature exposes the
// handful we need through test_exports. Extend test_exports in core
// if the set grows.
use skattr_core::test_exports::{ContactRepo, InsertParams, MessageRepo, Pool};

#[test]
fn pool_close_reopen_preserves_contacts_and_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = Seed::generate().unwrap();

    // First open: write a contact + a message.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let alice = Contact {
        identity: PublicKey([0x77; 32]),
        display_name: Some("alice".into()),
        added_at: 1700000000,
        card: None,
    };
    ContactRepo::new(&pool).upsert(&alice).unwrap();

    let env = Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1700000100,
        reply_to: None,
        kind: Kind::Text {
            body: "hello alice".into(),
        },
    };
    let gid = [0xAA; 32];
    let sender = [0x77; 32];
    MessageRepo::new(&pool)
        .insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: env.ts,
        })
        .unwrap();

    pool.close().unwrap();

    // Second open: same seed, read both back.
    let pool = Pool::open(tmp.path(), &seed).unwrap();
    let got = ContactRepo::new(&pool)
        .get(&alice.identity)
        .unwrap()
        .expect("alice survives close/reopen");
    assert_eq!(got.display_name, Some("alice".into()));

    let messages = MessageRepo::new(&pool).recent(&gid, 10).unwrap();
    assert_eq!(messages.len(), 1);
    let decoded = Envelope::decode(messages[0].body_blob.as_ref().unwrap()).unwrap();
    assert!(matches!(decoded.kind, Kind::Text { body } if body == "hello alice"));

    pool.close().unwrap();
}
