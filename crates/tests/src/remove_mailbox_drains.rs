// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Task 29 — `Command::RemoveMailbox` integration test.
//!
//! Pre-seeds a `'mine'` mailbox row with a deposited message, drives
//! `Command::RemoveMailbox`, and asserts:
//!
//!   * Two `Event::MailboxStatusChanged` events fire in order:
//!     `PendingRemoval` → `Removed`.
//!   * The row's persisted status ends as `Removed`.
//!   * The scheduler observes `PollerCtrl::RemoveMailbox(id)`.
//!   * With NO inbound dispatcher wired, the drain is skipped
//!     entirely: a prior deposit on the mailbox is PRESERVED
//!     server-side. The drain dispatch-then-delete path
//!     (`poll_dispatch_once`) only deletes deposits it could
//!     dispatch, and without a dispatcher we must not delete
//!     held offline messages on this irreversible removal path.
//!
//! This is the integration-level companion to the dispatch unit tests
//! in `core::daemon::dispatch`, which cover the same semantics from
//! inside the crate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::commands::{Command, CommandResult};
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::server::CommandExecutor;
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    daemon_handle_set_inbound, daemon_handle_with_mailbox, daemon_inbound_dispatch,
    delivery_hub_with_mailbox, mailbox_run_one_poll_tick, DaemonHandle, DeliveryHub, MailboxRepo,
    MailboxStatus, Pool, TestMailboxFactory, TestPollerCtrl,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

/// Replicate `mailbox::client::recipient_hash_from_pubkey` (which is
/// `pub(crate)`). The mailbox protocol defines the recipient hash as
/// `SHA-256(identity_pubkey)`.
fn recipient_hash(pk: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(pk).into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_mailbox_emits_status_events_and_drains_server() {
    let onion = "drain.onion";
    let mb = Arc::new(InProcessMailbox::new(onion));
    let factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb.clone());

    let pool = Arc::new(Pool::in_memory());
    let identity = IdentityKey::generate().unwrap();
    let identity_pk = identity.public();
    let (events_tx, mut events_rx) = broadcast::channel::<Event>(16);

    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        pool.clone(),
        None,
        events_tx.clone(),
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
    );

    let (handle, mut ctrl_rx): (
        Arc<DaemonHandle<tokio::io::DuplexStream>>,
        tokio::sync::mpsc::Receiver<TestPollerCtrl>,
    ) = daemon_handle_with_mailbox(
        pool.clone(),
        hub,
        identity,
        events_tx.clone(),
        factory.clone(),
    );
    handle.set_onion("self.onion".to_string());

    // Seed the row directly. (We bypass `Command::AddMailbox` so this
    // test is decoupled from the AddMailbox flow.)
    let mb_id = MailboxRepo::new(&pool).add_mine(onion, 0).unwrap();
    MailboxRepo::new(&pool)
        .mark_status(mb_id, MailboxStatus::Reachable)
        .unwrap();

    // Pre-deposit one ciphertext into the in-process server addressed
    // to `identity` so the drain has something to fetch.
    let recipient_hash_val = recipient_hash(&identity_pk.0);
    {
        use futures::{SinkExt, StreamExt};
        use skattr_core::mailbox::protocol::{Deposit, PROTOCOL_VERSION};
        use skattr_core::test_exports::{MailboxFrame, MailboxFrameCodec};
        use tokio_util::codec::Framed;

        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let server_for_seed = mb.server.clone();
        let server_task = tokio::spawn(async move {
            let _ = server_for_seed.accept_loop(server_side).await;
        });
        let mut framed = Framed::new(client_side, MailboxFrameCodec::new());
        framed
            .send(MailboxFrame::Deposit(Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: recipient_hash_val,
                ciphertext: b"seeded-ct".to_vec(),
                ttl_request: 86_400,
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::DepositOk(_)));
        drop(framed);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_task).await;
    }

    // ── Drive RemoveMailbox over the executor seam. ──────────────────
    let res = handle
        .execute(Command::RemoveMailbox { id: mb_id })
        .await
        .expect("RemoveMailbox must succeed");
    assert!(matches!(res, CommandResult::Ok));

    // Row must be Removed.
    let row = MailboxRepo::new(&pool).get(mb_id).unwrap().unwrap();
    assert_eq!(row.status, MailboxStatus::Removed);

    // Two MailboxStatusChanged events: PendingRemoval, then Removed.
    let ev1 = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
        .await
        .expect("event 1 arrives")
        .expect("event channel open");
    assert!(
        matches!(
            ev1,
            Event::MailboxStatusChanged {
                mailbox_id,
                status: MailboxStatus::PendingRemoval
            } if mailbox_id == mb_id
        ),
        "expected PendingRemoval, got {ev1:?}"
    );
    let ev2 = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
        .await
        .expect("event 2 arrives")
        .expect("event channel open");
    assert!(
        matches!(
            ev2,
            Event::MailboxStatusChanged {
                mailbox_id,
                status: MailboxStatus::Removed
            } if mailbox_id == mb_id
        ),
        "expected Removed, got {ev2:?}"
    );

    // Scheduler observed RemoveMailbox(id).
    let ctrl_msg = tokio::time::timeout(std::time::Duration::from_secs(1), ctrl_rx.recv())
        .await
        .expect("ctrl arrives within 1 s")
        .expect("ctrl channel open");
    assert_eq!(ctrl_msg, TestPollerCtrl::RemoveMailbox(mb_id));

    // Preservation proof: this handle has NO inbound dispatcher wired, so
    // RemoveMailbox skips the drain entirely (it cannot dispatch deposits,
    // and must not delete what it cannot dispatch on this irreversible
    // path). A fresh poll against the same in-process server still returns
    // our seeded ciphertext — it was preserved, not lost.
    let deposits = mailbox_run_one_poll_tick(factory.as_ref(), onion, &handle.identity)
        .await
        .unwrap();
    assert_eq!(
        deposits.len(),
        1,
        "with no inbound wired, drain is skipped and held deposits are preserved"
    );
}

/// Task 22.5 — RemoveMailbox drain must DISPATCH held deposits into local
/// storage (not merely server-side delete them).
///
/// Setup: the daemon owner ("Bob") shares a 2-member MLS group with "Alice".
/// Alice encrypts an envelope and deposits the resulting MLS ciphertext into
/// the in-process mailbox Bob is about to remove. `RemoveMailbox` should
/// fetch that deposit and run it through the real `DaemonInbound`, which
/// trial-decrypts, persists, and emits `Event::MessageReceived` — so the
/// offline message survives mailbox removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_mailbox_drain_dispatches_held_deposit_into_storage() {
    use skattr_core::contact::{Contact, ContactCard};
    use skattr_core::envelope::{Envelope, Kind, MessageId};
    use skattr_core::test_exports::{
        ContactRepo, Group, KeyPackage, KeyPackageRepo, MlsGroupRepo, MlsProvider,
    };

    let onion = "drain-dispatch.onion";
    let mb = Arc::new(InProcessMailbox::new(onion));
    let factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb.clone());

    // ── Identities + the daemon owner's pool ────────────────────────────
    let bob_id = IdentityKey::generate().unwrap(); // daemon owner / recipient
    let alice_id = IdentityKey::generate().unwrap(); // remote sender
    let pool = Arc::new(Pool::in_memory());
    let bob_pk = bob_id.public();
    let alice_pk = alice_id.public();

    // ── 2-member MLS group: Alice + Bob, sharing the same PSK. ──────────
    let psk = [0xA5u8; 32];
    let kp_ref = [7u8; 32];
    let bob_kp_repo = KeyPackageRepo::new(&pool);
    let bob_provider = MlsProvider::new();
    let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &bob_kp_repo).unwrap();

    let mut alice_group =
        Group::create_solo(&alice_id, Some((&kp_ref, &psk)), None, MlsProvider::new()).unwrap();
    let (welcome, _commit) = alice_group
        .add_member(&bob_kp, Some((&kp_ref, &psk)), None)
        .unwrap();
    let bob_group =
        Group::join_from_welcome(&bob_id, &welcome, Some((&kp_ref, &psk)), None, bob_provider)
            .unwrap();
    // Persist Bob's group in his pool so DaemonInbound can resolve + decrypt.
    bob_group.save(&MlsGroupRepo::new(&pool)).unwrap();

    // Simulate Welcome-Ack: alice_group (committer) stays PendingJoin until
    // the peer Acks (#93). set_active() restores the established-group
    // precondition so the encrypt() call below succeeds.
    alice_group.set_active();
    let group_id_bytes = bob_group.id().0.clone();

    // Install Alice as a contact in Bob's pool, linked to the shared group,
    // so `contact_for_group` resolves the sender during mailbox dispatch.
    {
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: alice_pk,
                display_name: Some("alice".into()),
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts.set_group_id(&alice_pk, &group_id_bytes).unwrap();
        let card = ContactCard::sign(
            &alice_id,
            "alice.onion".to_string(),
            vec![],
            1,
            24 * 3600,
            0,
        )
        .unwrap();
        contacts.put_card(&card).unwrap();
    }

    // ── Alice encrypts an envelope; we deposit the ciphertext. ──────────
    let body_text = "offline message that must survive mailbox removal";
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

    // ── Build the daemon handle for Bob with mailbox + real inbound. ────
    let (events_tx, mut events_rx) = broadcast::channel::<Event>(32);
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        pool.clone(),
        None,
        events_tx.clone(),
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
    );
    let (mut handle, _ctrl_rx): (
        Arc<DaemonHandle<tokio::io::DuplexStream>>,
        tokio::sync::mpsc::Receiver<TestPollerCtrl>,
    ) = daemon_handle_with_mailbox(
        pool.clone(),
        hub,
        bob_id,
        events_tx.clone(),
        factory.clone(),
    );

    // Inject the real DaemonInbound so the drain can dispatch deposits. The
    // dispatcher identity is only used for Welcome processing, not MLS app
    // decrypt (which uses the group ratchet), so a throwaway identity is fine.
    let inbound = daemon_inbound_dispatch(
        pool.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
        events_tx.clone(),
    );
    {
        let h = Arc::get_mut(&mut handle).expect("unique handle Arc before sharing");
        daemon_handle_set_inbound(h, inbound);
    }
    handle.set_onion("self.onion".to_string());

    // ── Seed the row + deposit Alice's ciphertext. ──────────────────────
    let mb_id = MailboxRepo::new(&pool).add_mine(onion, 0).unwrap();
    MailboxRepo::new(&pool)
        .mark_status(mb_id, MailboxStatus::Reachable)
        .unwrap();
    deposit_ciphertext(&mb, &bob_pk.0, &ciphertext).await;

    // ── Drive RemoveMailbox. ────────────────────────────────────────────
    let res = handle
        .execute(Command::RemoveMailbox { id: mb_id })
        .await
        .expect("RemoveMailbox must succeed");
    assert!(matches!(res, CommandResult::Ok));

    // ── The held deposit must now be persisted + a MessageReceived event
    //    fired (proof the drain dispatched it, not just deleted it). ─────
    let mut saw_message = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), events_rx.recv()).await {
            Ok(Ok(Event::MessageReceived { record, .. })) => {
                if matches!(&record.kind, Kind::Text { body } if body == body_text) {
                    saw_message = true;
                    break;
                }
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(
        saw_message,
        "RemoveMailbox drain must dispatch the held deposit (MessageReceived with the body)"
    );
}

/// Deposit a ciphertext addressed to `recipient_pk` into the in-process
/// mailbox `mb`, driving one Deposit frame over a fresh duplex stream.
async fn deposit_ciphertext(
    mb: &Arc<InProcessMailbox>,
    recipient_pk: &[u8; 32],
    ciphertext: &[u8],
) {
    use futures::{SinkExt, StreamExt};
    use skattr_core::mailbox::protocol::{Deposit, PROTOCOL_VERSION};
    use skattr_core::test_exports::{MailboxFrame, MailboxFrameCodec};
    use tokio_util::codec::Framed;

    let recipient_hash_val = recipient_hash(recipient_pk);
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let server_for_seed = mb.server.clone();
    let server_task = tokio::spawn(async move {
        let _ = server_for_seed.accept_loop(server_side).await;
    });
    let mut framed = Framed::new(client_side, MailboxFrameCodec::new());
    framed
        .send(MailboxFrame::Deposit(Deposit {
            version: PROTOCOL_VERSION,
            recipient_hash: recipient_hash_val,
            ciphertext: ciphertext.to_vec(),
            ttl_request: 86_400,
        }))
        .await
        .unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    assert!(matches!(resp, MailboxFrame::DepositOk(_)));
    drop(framed);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_task).await;
}
