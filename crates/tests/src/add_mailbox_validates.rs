// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Task 28 — `Command::AddMailbox` integration test over the
//! `daemon_handle_with_mailbox` harness.
//!
//! Drives:
//!   * Reachable mailbox case → `CommandResult::Ok`, row persisted as
//!     `Reachable`, scheduler observes `PollerCtrl::AddMailbox(id)`.
//!   * Unreachable mailbox case → `IpcError::Daemon(InvalidArgument
//!     {message: "unreachable"})`, no row persisted.
//!
//! Where dispatch unit tests in `core::daemon::dispatch` cover the same
//! semantics from inside the crate, this test exercises the
//! cross-crate `DaemonHandle::execute` seam used by the IPC layer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::commands::{Command, CommandResult};
use skattr_core::daemon::error_kind::DaemonErrorKind;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::server::CommandExecutor;
use skattr_core::daemon::ipc::wire::IpcError;
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    daemon_handle_with_mailbox, delivery_hub_with_mailbox, DaemonHandle, DeliveryHub, MailboxRepo,
    MailboxStatus, Pool, TestMailboxFactory, TestPollerCtrl,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

/// Build a daemon handle wired to `factory`. Returns the handle, an
/// observer for `PollerCtrl` notifications, and the underlying pool so
/// the caller can assert on `mailboxes` rows.
#[allow(clippy::type_complexity)]
fn build_handle(
    factory: Arc<dyn TestMailboxFactory>,
) -> (
    Arc<DaemonHandle<tokio::io::DuplexStream>>,
    tokio::sync::mpsc::Receiver<TestPollerCtrl>,
    Arc<Pool>,
    broadcast::Sender<Event>,
) {
    let pool = Arc::new(Pool::in_memory());
    let identity = IdentityKey::generate().unwrap();
    let (events_tx, _) = broadcast::channel::<Event>(16);

    // Hub is built without mailbox fallback for this test — AddMailbox
    // doesn't reach into the hub.
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        pool.clone(),
        None,
        events_tx.clone(),
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
    );

    let (handle, ctrl_rx) = daemon_handle_with_mailbox(
        pool.clone(),
        hub,
        identity,
        events_tx.clone(),
        factory,
    );
    handle.set_onion("self.onion".to_string());
    (handle, ctrl_rx, pool, events_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_mailbox_reachable_persists_and_notifies_scheduler() {
    let onion = "live.onion";
    let mb = Arc::new(InProcessMailbox::new(onion));
    let factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb);
    let (handle, mut ctrl_rx, pool, _events_tx) = build_handle(factory);

    let res = handle
        .execute(Command::AddMailbox {
            onion: onion.into(),
        })
        .await
        .expect("AddMailbox must succeed against a reachable mailbox");
    assert!(matches!(res, CommandResult::Ok));

    // Row persisted as Reachable.
    let rows = MailboxRepo::new(&pool).list_mine().unwrap();
    assert_eq!(rows.len(), 1, "exactly one 'mine' row expected");
    assert_eq!(rows[0].onion, onion);
    assert_eq!(
        rows[0].status,
        MailboxStatus::Reachable,
        "post-probe status must be Reachable"
    );

    // Scheduler must have received PollerCtrl::AddMailbox(id).
    let ctrl_msg = tokio::time::timeout(std::time::Duration::from_secs(1), ctrl_rx.recv())
        .await
        .expect("ctrl message arrives within 1 s")
        .expect("ctrl channel still open");
    assert_eq!(
        ctrl_msg,
        TestPollerCtrl::AddMailbox(rows[0].id),
        "scheduler must observe AddMailbox(id) for the new row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_mailbox_unreachable_returns_invalid_argument_and_does_not_persist() {
    // Empty factory — every connect returns Unreachable.
    let factory: Arc<dyn TestMailboxFactory> =
        Arc::new(InProcessMailboxFactory::new(Vec::new()));
    let (handle, _ctrl_rx, pool, _events_tx) = build_handle(factory);

    let err = handle
        .execute(Command::AddMailbox {
            onion: "dead.onion".into(),
        })
        .await
        .expect_err("AddMailbox must fail against an unreachable mailbox");
    match err {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument { message }) => {
            assert_eq!(message, "unreachable", "expected reason 'unreachable'");
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }

    // No row persisted on the failure path.
    let rows = MailboxRepo::new(&pool).list_mine().unwrap();
    assert!(
        rows.is_empty(),
        "no 'mine' row may persist after a failed probe"
    );
}
