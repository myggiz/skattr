// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Task 26 — Mailbox failover.
//!
//! Bob's `ContactCard` advertises two mailboxes. The first one fails
//! every connect; the second accepts. `DeliveryHub::ensure_mailbox_fallback`
//! must walk the list and deposit into the second mailbox — leaving
//! the originating outbox row deleted exactly once and emitting a single
//! `DeliveryStatusChanged{Deposited}` event.
//!
//! Builds on top of the same harness as `mailbox_offline_delivery`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::contact::{Contact, ContactCard};
use skattr_core::daemon::events::{DeliveryStatus, Event};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    delivery_hub_with_mailbox, mailbox_run_one_poll_tick, outbox_count_for_target,
    outbox_seed_direct, ContactRepo, DeliveryHub, Group, KeyPackage, KeyPackageRepo, MlsGroupRepo,
    MlsProvider, Pool, TestMailboxFactory,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

fn paired_groups(
    alice_id: &IdentityKey,
    bob_id: &IdentityKey,
    alice_pool: &Pool,
    bob_pool: &Pool,
) -> (Group, Group) {
    let psk = [0x99u8; 32];
    let kp_ref = [7u8; 32]; // ADR 0009: fixed test KeyPackageRef for the PSK id
    let bob_kp_repo = KeyPackageRepo::new(bob_pool);
    let bob_provider = MlsProvider::new();
    let bob_kp = KeyPackage::generate(bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice =
        Group::create_solo(alice_id, Some((&kp_ref, &psk)), None, MlsProvider::new()).unwrap();
    let (welcome, _commit) = alice
        .add_member(&bob_kp, Some((&kp_ref, &psk)), None)
        .unwrap();
    let bob = Group::join_from_welcome(bob_id, &welcome, Some((&kp_ref, &psk)), None, bob_provider)
        .unwrap();

    // Simulate Welcome-Ack: committer stays PendingJoin until the peer Acks
    // (#93). set_active() restores the established-group precondition so the
    // encrypt() call below succeeds. Bob (joiner) is already Active.
    alice.set_active();

    alice.save(&MlsGroupRepo::new(alice_pool)).unwrap();
    bob.save(&MlsGroupRepo::new(bob_pool)).unwrap();
    (alice, bob)
}

fn install_contact_with_card(
    pool: &Pool,
    peer: &IdentityKey,
    onion: &str,
    mailboxes: Vec<String>,
    group_id: &[u8],
) {
    let contacts = ContactRepo::new(pool);
    contacts
        .upsert(&Contact {
            identity: peer.public(),
            display_name: Some("peer".into()),
            added_at: 0,
            card: None,
            muted: false,
        })
        .unwrap();
    contacts.set_group_id(&peer.public(), group_id).unwrap();

    let card = ContactCard::sign(peer, onion.to_string(), mailboxes, 1, 24 * 3600, 0).unwrap();
    contacts.put_card(&card).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn primary_unreachable_secondary_accepts() {
    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());

    let (mut alice_group, mut bob_group) =
        paired_groups(&alice_id, &bob_id, &alice_pool, &bob_pool);
    let group_id_bytes = alice_group.id().0.clone();

    // ── Two in-process mailboxes. Primary always fails connects;
    //    secondary accepts. ────────────────────────────────────────────
    let primary_onion = "primary.onion";
    let secondary_onion = "secondary.onion";
    let primary = Arc::new(InProcessMailbox::new(primary_onion));
    let secondary = Arc::new(InProcessMailbox::new(secondary_onion));
    primary.fail_next_connects();

    let factory: Arc<dyn TestMailboxFactory> = Arc::new(InProcessMailboxFactory::new(vec![
        primary.clone(),
        secondary.clone(),
    ]));

    // Bob's card lists primary FIRST, then secondary.
    install_contact_with_card(
        &alice_pool,
        &bob_id,
        "bobs.onion",
        vec![primary_onion.to_string(), secondary_onion.to_string()],
        &group_id_bytes,
    );

    let (events_tx, mut events_rx) = broadcast::channel::<Event>(16);
    let alice_hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        alice_pool.clone(),
        None,
        events_tx,
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
    );

    // Encrypt one message + seed an outbox row.
    let msg_id = MessageId::generate();
    let body_text = "failover-payload";
    let envelope = Envelope {
        v: 1,
        id: msg_id,
        ts: 0,
        reply_to: None,
        kind: Kind::Text {
            body: body_text.into(),
        },
    };
    let ciphertext = alice_group.encrypt(&envelope).unwrap();
    alice_group.save(&MlsGroupRepo::new(&alice_pool)).unwrap();
    outbox_seed_direct(&alice_pool, &bob_id.public().0, &msg_id.0, &ciphertext).unwrap();

    // ── Drive fallback. Hub must walk past the failing primary and
    //    deposit into the secondary. ─────────────────────────────────
    alice_hub
        .ensure_mailbox_fallback(bob_id.public(), msg_id, ciphertext.clone())
        .await;

    // Outbox row deleted exactly once.
    assert_eq!(
        outbox_count_for_target(&alice_pool, &bob_id.public().0).unwrap(),
        0,
        "outbox row must be deleted after a successful failover deposit"
    );

    // Single Deposited event.
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
        .await
        .expect("Deposited event arrives within 2 s")
        .expect("event channel still open");
    match evt {
        Event::DeliveryStatusChanged {
            message,
            status: DeliveryStatus::Deposited,
        } => assert_eq!(message, msg_id),
        other => panic!("expected DeliveryStatusChanged{{Deposited}}, got {other:?}"),
    }
    // No further events.
    let extra = tokio::time::timeout(std::time::Duration::from_millis(200), events_rx.recv()).await;
    assert!(
        extra.is_err(),
        "exactly one Deposited event expected; got a second"
    );

    // ── Primary mailbox must contain zero deposits — every connect
    //    failed. Secondary must contain exactly one. ─────────────────
    //
    // Allow connects to the primary again so we can poll-and-confirm
    // it is empty. (The harness's `fail_connects` flag is process-wide
    // for the mailbox; flipping it back is safe now that the deposit
    // walk has completed.)
    primary.allow_connects();

    let primary_deposits = mailbox_run_one_poll_tick(factory.as_ref(), primary_onion, &bob_id)
        .await
        .unwrap();
    assert!(
        primary_deposits.is_empty(),
        "primary mailbox must be empty (every connect failed)"
    );

    let secondary_deposits = mailbox_run_one_poll_tick(factory.as_ref(), secondary_onion, &bob_id)
        .await
        .unwrap();
    assert_eq!(
        secondary_deposits.len(),
        1,
        "secondary mailbox must hold the single failover deposit"
    );

    // ── Decrypt + assert round-trip ──────────────────────────────────
    let got = bob_group
        .decrypt(&secondary_deposits[0].ciphertext)
        .unwrap()
        .expect("app message");
    assert_eq!(got.id, msg_id, "envelope id must match after failover");
    match got.kind {
        Kind::Text { body } => assert_eq!(body, body_text),
        other => panic!("unexpected kind: {other:?}"),
    }
}
