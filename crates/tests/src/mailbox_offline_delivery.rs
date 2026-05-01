// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Task 25 — End-to-end mailbox offline delivery.
//!
//! Alice sends a message to Bob while Bob is offline. Direct delivery
//! is impossible (no peer connection installed), so the test invokes
//! the `DeliveryHub` direct→mailbox fallback orchestrator. This:
//!
//! 1. Looks up Bob's `ContactCard` in Alice's storage and reads the
//!    advertised mailbox onion list.
//! 2. Picks one (deterministically per message id).
//! 3. Connects to the in-process [`MailboxServer`] via the harness's
//!    [`InProcessMailboxFactory`].
//! 4. Deposits the ciphertext.
//! 5. Deletes the originating direct outbox row + emits
//!    `Event::DeliveryStatusChanged{Deposited}`.
//!
//! Bob's side then drives one [`run_one_poll_tick`] cycle against the
//! same in-process server to fetch and delete the deposit. The test
//! decrypts the fetched ciphertext through Bob's MLS group to confirm
//! the round-trip preserves the original envelope.
//!
//! ## Out of scope
//!
//! - The full `PollScheduler` supervisor / actor loop. Tests that
//!   exercise the scheduler land in Task 27 + Task 32 (real-Tor).
//! - The MLS-decrypt side of `InboundDispatch` plumbed through the
//!   scheduler. Bob's pool already holds his Group; this test calls
//!   `Group::decrypt` directly on the fetched ciphertext.
//! - Symmetric Welcome handoff. Both Alice and Bob are constructed
//!   with the same `psk`-keyed 2-member group from the harness.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::contact::{Contact, ContactCard};
use skattr_core::daemon::events::{DeliveryStatus, Event};
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    delivery_hub_with_mailbox, mailbox_run_one_poll_tick, outbox_count_for_target,
    outbox_seed_direct, ContactRepo, DeliveryHub, Group, KeyPackage, KeyPackageRepo,
    MlsGroupRepo, MlsProvider, Pool, TestMailboxFactory,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

/// Build a 2-member MLS group between Alice and Bob, sharing the same
/// psk. Returns the matching `Group` instances; both pools have the
/// `MlsGroupRepo` snapshot persisted.
#[allow(clippy::type_complexity)]
fn paired_groups(
    alice_id: &IdentityKey,
    bob_id: &IdentityKey,
    alice_pool: &Pool,
    bob_pool: &Pool,
) -> (Group, Group) {
    let psk = [0xA5u8; 32];
    let bob_kp_repo = KeyPackageRepo::new(bob_pool);
    let bob_provider = MlsProvider::new();
    let bob_kp = KeyPackage::generate(bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice = Group::create_solo(alice_id, Some(&psk), MlsProvider::new()).unwrap();
    let (welcome, _commit) = alice.add_member(&bob_kp, Some(&psk)).unwrap();
    let bob = Group::join_from_welcome(bob_id, &welcome, Some(&psk), bob_provider).unwrap();

    let alice_repo = MlsGroupRepo::new(alice_pool);
    let bob_repo = MlsGroupRepo::new(bob_pool);
    alice.save(&alice_repo).unwrap();
    bob.save(&bob_repo).unwrap();
    (alice, bob)
}

/// Insert a contact row + verified ContactCard for `peer` into
/// `pool`'s `contacts` / `contact_cards` tables. Required so
/// `MailboxRepo::list_for_contact` and `ContactRepo::get_group_id` can
/// resolve Bob from Alice's perspective.
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
        })
        .unwrap();
    contacts.set_group_id(&peer.public(), group_id).unwrap();

    let card = ContactCard::sign(
        peer,
        onion.to_string(),
        mailboxes,
        1, // version
        24 * 3600,
        0,
    )
    .unwrap();
    contacts.put_card(&card).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn alice_deposits_to_mailbox_bob_fetches_then_decrypts() {
    // ── Identities + pools ───────────────────────────────────────────
    let alice_id = IdentityKey::generate().unwrap();
    let bob_id = IdentityKey::generate().unwrap();
    let alice_pool = Arc::new(Pool::in_memory());
    let bob_pool = Arc::new(Pool::in_memory());

    // ── 2-member MLS group on both sides ─────────────────────────────
    let (mut alice_group, mut bob_group) =
        paired_groups(&alice_id, &bob_id, &alice_pool, &bob_pool);
    let group_id_bytes = alice_group.id().0.clone();

    // ── In-process mailbox + factory ─────────────────────────────────
    let mailbox_onion = "bobsmailbox.onion";
    let mb = Arc::new(InProcessMailbox::new(mailbox_onion));
    let factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb.clone());

    // ── Wire Alice's storage so list_for_contact returns the mailbox
    //    onion for Bob. Bob's ContactCard advertises mailbox_onion.
    install_contact_with_card(
        &alice_pool,
        &bob_id,
        "bobs.onion",
        vec![mailbox_onion.to_string()],
        &group_id_bytes,
    );

    // ── Build Alice's hub with mailbox fallback wired in ─────────────
    let (alice_events_tx, mut alice_events_rx) = broadcast::channel::<Event>(16);
    let alice_hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        alice_pool.clone(),
        None, // no inbound dispatch needed on Alice for this test
        alice_events_tx,
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()), // unused at deposit time; see hub::MailboxFallback comment
    );

    // ── Encrypt one envelope on Alice's side ─────────────────────────
    let body_text = "hello from alice while bob is offline";
    let msg_id = MessageId::generate();
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
    alice_group
        .save(&MlsGroupRepo::new(&alice_pool))
        .unwrap();

    // ── Seed the direct outbox row that fallback retargets ───────────
    outbox_seed_direct(&alice_pool, &bob_id.public().0, &msg_id.0, &ciphertext)
        .unwrap();
    assert_eq!(
        outbox_count_for_target(&alice_pool, &bob_id.public().0).unwrap(),
        1,
        "direct outbox row must be present before fallback runs"
    );

    // ── Drive the mailbox fallback ───────────────────────────────────
    alice_hub
        .ensure_mailbox_fallback(bob_id.public(), msg_id, ciphertext.clone())
        .await;

    // Outbox row must be deleted on success.
    assert_eq!(
        outbox_count_for_target(&alice_pool, &bob_id.public().0).unwrap(),
        0,
        "successful deposit must delete the direct outbox row"
    );

    // Alice must observe Event::DeliveryStatusChanged{Deposited}.
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), alice_events_rx.recv())
        .await
        .expect("Deposited event arrives within 2 s")
        .expect("event channel still open");
    match evt {
        Event::DeliveryStatusChanged {
            message,
            status: DeliveryStatus::Deposited,
        } => {
            assert_eq!(message, msg_id, "event must reference the deposited msg");
        }
        other => panic!("expected DeliveryStatusChanged{{Deposited}}, got {other:?}"),
    }

    // ── Bob comes online: drive one poll tick. ───────────────────────
    //
    // The harness's `mailbox_run_one_poll_tick` opens a fresh client
    // connection through `factory`, runs Challenge → Fetch → Delete,
    // and returns the pending deposits.
    let deposits = mailbox_run_one_poll_tick(factory.as_ref(), mailbox_onion, &bob_id)
        .await
        .unwrap();
    assert_eq!(deposits.len(), 1, "exactly one deposit expected for Bob");

    // ── Decrypt the fetched ciphertext through Bob's MLS group. ──────
    let fetched_ct = &deposits[0].ciphertext;
    let got = bob_group.decrypt(fetched_ct).unwrap();
    assert_eq!(got.id, msg_id, "round-tripped envelope id must match");
    match got.kind {
        Kind::Text { body } => {
            assert_eq!(body, body_text, "decrypted body must match original");
        }
        other => panic!("expected Kind::Text, got {other:?}"),
    }
    bob_group.save(&MlsGroupRepo::new(&bob_pool)).unwrap();

    // ── A second poll tick must return zero deposits (server-side
    //    delete worked) — proves exactly-once mailbox delivery. ───────
    let deposits2 = mailbox_run_one_poll_tick(factory.as_ref(), mailbox_onion, &bob_id)
        .await
        .unwrap();
    assert_eq!(
        deposits2.len(),
        0,
        "second poll must return zero deposits (server-side delete worked)"
    );
}
