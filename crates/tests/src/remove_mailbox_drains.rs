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
//!   * The drain step actually contacted the in-process server: a
//!     prior deposit on the mailbox is gone after the command
//!     completes (drain runs `run_one_poll_tick`, which deletes
//!     server-side).
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
    daemon_handle_with_mailbox, delivery_hub_with_mailbox, mailbox_run_one_poll_tick, DaemonHandle,
    DeliveryHub, MailboxRepo, MailboxStatus, Pool, TestMailboxFactory, TestPollerCtrl,
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

    // Drain proof: a fresh poll against the same in-process server
    // returns zero deposits — the drain step inside RemoveMailbox
    // server-side-deleted our seeded ciphertext.
    let deposits = mailbox_run_one_poll_tick(factory.as_ref(), onion, &handle.identity)
        .await
        .unwrap();
    assert!(
        deposits.is_empty(),
        "drain must server-side delete pre-existing deposits"
    );
}
