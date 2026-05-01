// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Task 27 — `Command::RotateOnion` integration test (degenerate cut).
//!
//! ## Status
//!
//! `Command::RotateOnion` is currently a **degenerate handler** that
//! republishes the self-card without performing real HS key rotation.
//! Real HS key rotation lands in Task 23.5; the full grace-period
//! test (including offline-recipient catchup via mailbox) is parked
//! against that follow-up.
//!
//! Until Task 23.5 lands, this test exercises only the visible
//! end-to-end behaviour the degenerate handler is responsible for:
//!
//!   * `Command::RotateOnion` over the IPC executor seam returns `Ok`.
//!   * The persisted `self_card_state.version` counter advances by
//!     exactly 1, proving the card-publish side-effect ran.
//!
//! When Task 23.5 ships, this file should be extended to verify:
//!
//!   * Old onion still reachable for the grace window.
//!   * New onion reachable.
//!   * Offline contact's mailbox receives the new ContactCard via the
//!     same fallback orchestrator that `mailbox_offline_delivery`
//!     exercises.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::commands::{Command, CommandResult};
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::server::CommandExecutor;
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    daemon_handle_with_mailbox, delivery_hub_with_mailbox, DaemonHandle, DeliveryHub, Pool,
    TestMailboxFactory,
};
use tokio::sync::broadcast;

use crate::mailbox_harness::InProcessMailboxFactory;

/// Read `self_card_state.version` directly from the pool. Mirrors the
/// helper used in `core::daemon::dispatch` unit tests; replicated here
/// because the dispatch helper is `pub(crate)`. We can't reach
/// `pool.with` (also `pub(crate)`) from here, so we go via the
/// `test_exports::self_card_state_version` helper.
fn read_self_card_version(pool: &Pool) -> u64 {
    skattr_core::test_exports::self_card_state_version(pool).unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotate_onion_bumps_self_card_version() {
    // Empty factory — no mailbox required for the degenerate handler;
    // it only needs `onion()` set.
    let factory: Arc<dyn TestMailboxFactory> = Arc::new(InProcessMailboxFactory::new(Vec::new()));

    let pool = Arc::new(Pool::in_memory());
    let identity = IdentityKey::generate().unwrap();
    let (events_tx, _) = broadcast::channel::<Event>(16);

    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = delivery_hub_with_mailbox(
        pool.clone(),
        None,
        events_tx.clone(),
        factory.clone(),
        Arc::new(IdentityKey::generate().unwrap()),
    );

    let (handle, _ctrl_rx): (Arc<DaemonHandle<tokio::io::DuplexStream>>, _) =
        daemon_handle_with_mailbox(pool.clone(), hub, identity, events_tx, factory);
    handle.set_onion("rotate-test.onion".to_string());

    let version_before = read_self_card_version(&pool);

    let res = handle
        .execute(Command::RotateOnion)
        .await
        .expect("RotateOnion must succeed");
    assert!(matches!(res, CommandResult::Ok));

    let version_after = read_self_card_version(&pool);
    assert_eq!(
        version_after,
        version_before + 1,
        "self_card_state.version must be bumped by RotateOnion"
    );

    // TODO Task 23.5: extend this test to assert that an offline
    // contact's mailbox receives the new ContactCard once real HS
    // key rotation lands.
}
