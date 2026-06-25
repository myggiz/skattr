// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.C guardrail: peer offline -> direct fails -> timeout -> fallback
//! deposit to mailbox -> recipient polls -> receives & decrypts. Exercises the
//! real `run_with_transport` assembly (sweeper + per-peer trigger + poll).
//!
//! ## Scenario
//!
//! Two daemons (Alice = sender, Bob = recipient) boot over the in-process
//! `LoopbackTransport` via `test_exports::run_loopback_with_mailbox` — the
//! production `run_with_transport` assembly with Arti swapped for loopback and
//! a REAL in-process mailbox server (shared by both daemons) swapped in for
//! `ArtiMailboxFactory`. Nothing about the delivery path is hand-wired.
//!
//! `seed_offline_pair` establishes a shared 2-member MLS group and mutual
//! contacts, but makes Bob DIRECT-UNREACHABLE: Alice's `ContactCard` for Bob
//! advertises an onion that is never published on the `LoopbackNet`, so every
//! direct dial fails immediately. The same card advertises Bob's mailbox onion,
//! and Bob owns a `'mine'` row pointing at it, so his `PollScheduler` polls it.
//!
//! ## Timing — why this is deterministic and which knobs are set
//!
//! - `direct_timeout_secs = 1` (via config): the per-peer actor's
//!   `run_mailbox_fallback` trigger fires ~1 s after Alice's first failed dial.
//!   That FIRST deposit succeeds (the mailbox is reachable), so the message
//!   reaches the mailbox at trigger time — we do NOT depend on the 15 s
//!   mailbox-outbox sweeper (which is only the retry path).
//! - `PollCadence::fast()` (wired by `run_loopback_with_mailbox`, gated on
//!   `feature = "test-harness"`): Bob's poll actor ticks sub-second instead of
//!   the production 45-75 s idle interval, so it fetches the deposit promptly.
//!   Production cadence (60 s idle / 15 s active) is unchanged.
//!
//! The whole chain therefore converges in a few seconds; all waits are
//! deadline-bounded polls, never fixed sleeps.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::envelope::Kind;
use skattr_core::test_exports::{
    run_loopback_with_mailbox, seed_offline_pair, LoopbackNet, TestMailboxFactory,
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::loopback_harness::{config_for, init_vault, subscribe_messages, wait_for_message};
use crate::mailbox_harness::{InProcessMailbox, InProcessMailboxFactory};

/// Onion Bob's daemon publishes on the loopback net (so Bob is alive).
const BOB_ONION: &str = "bob-offline.onion";
/// Onion Alice's ContactCard for Bob advertises — deliberately NOT published
/// on the loopback net, so Alice's direct dial to Bob always fails.
const BOB_UNREACHABLE_ONION: &str = "bob-never-published.onion";
/// Alice's published onion.
const ALICE_ONION: &str = "alice-offline.onion";
/// The in-process mailbox both daemons route to.
const MAILBOX_ONION: &str = "shared-mailbox.onion";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_peer_receives_via_mailbox_fallback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    // Seed established contacts with Bob direct-unreachable but mailbox-backed.
    let pw_seed = Zeroizing::new(crate::loopback_harness::PASSPHRASE.to_string());
    let (alice_pub, bob_pub) = seed_offline_pair(
        tmp_a.path(),
        tmp_b.path(),
        &pw_seed,
        ALICE_ONION,
        BOB_UNREACHABLE_ONION,
        MAILBOX_ONION,
    )
    .expect("seed offline pair");

    // ONE in-process mailbox server, shared by both daemons' factories so
    // Alice's deposit and Bob's fetch hit the same `Store`.
    let mb = Arc::new(InProcessMailbox::new(MAILBOX_ONION));
    let test_factory: Arc<dyn TestMailboxFactory> = InProcessMailboxFactory::single(mb.clone());
    let factory_a = test_factory.clone();
    let factory_b = test_factory.clone();

    let net = LoopbackNet::new();
    let pw = Zeroizing::new(crate::loopback_harness::PASSPHRASE.to_string());

    // Both daemons run with direct_timeout_secs = 1 so the per-peer fallback
    // trigger fires ~1 s after Alice's first failed dial.
    let mut a_cfg = config_for(tmp_a.path());
    a_cfg.delivery.direct_timeout_secs = 1;
    let mut b_cfg = config_for(tmp_b.path());
    b_cfg.delivery.direct_timeout_secs = 1;

    // --- Alice ---
    let (ready_a_tx, ready_a_rx) = oneshot::channel();
    let (shutdown_a_tx, shutdown_a_rx) = oneshot::channel::<()>();
    let a_dir = tmp_a.path().to_path_buf();
    let a_net = net.clone();
    let a_pw = pw.clone();
    let task_a = tokio::spawn(async move {
        run_loopback_with_mailbox(
            &a_dir,
            &a_pw,
            a_cfg,
            std::path::PathBuf::from("/dev/null"),
            a_net,
            ALICE_ONION.into(),
            factory_a,
            ready_a_tx,
            async move {
                let _ = shutdown_a_rx.await;
            },
        )
        .await
    });

    // --- Bob ---
    let (ready_b_tx, ready_b_rx) = oneshot::channel();
    let (shutdown_b_tx, shutdown_b_rx) = oneshot::channel::<()>();
    let b_dir = tmp_b.path().to_path_buf();
    let b_net = net.clone();
    let b_pw = pw.clone();
    let task_b = tokio::spawn(async move {
        run_loopback_with_mailbox(
            &b_dir,
            &b_pw,
            b_cfg,
            std::path::PathBuf::from("/dev/null"),
            b_net,
            BOB_ONION.into(),
            factory_b,
            ready_b_tx,
            async move {
                let _ = shutdown_b_rx.await;
            },
        )
        .await
    });

    let ready_a = tokio::time::timeout(Duration::from_secs(60), ready_a_rx)
        .await
        .expect("Alice ready within 30 s")
        .expect("Alice ready_tx open");
    let ready_b = tokio::time::timeout(Duration::from_secs(60), ready_b_rx)
        .await
        .expect("Bob ready within 30 s")
        .expect("Bob ready_tx open");

    // Sanity: each side advertises the onion we expect.
    assert_eq!(ready_a.onion, ALICE_ONION);
    assert_eq!(ready_b.onion, BOB_ONION);

    // Subscribe on Bob BEFORE Alice sends so the MessageReceived (fired from
    // the poll-dispatch path) cannot be missed.
    let mut bob_sub = subscribe_messages(&ready_b.ipc_socket, alice_pub).await;

    // Alice sends. The direct dial fails (Bob's card onion is unpublished);
    // the SendMessage call returns once the row is persisted + handed off —
    // it does NOT block on delivery. After ~1 s the per-peer trigger deposits
    // to the mailbox; Bob's fast poll then fetches + dispatches it.
    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(30),
        send_a.execute(Command::SendMessage {
            contact: bob_pub,
            kind: Kind::Text {
                body: "offline-hello".into(),
            },
        }),
    )
    .await
    .expect("Alice send returns within 30 s")
    .unwrap()
    {
        CommandResult::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {other:?}"),
    }

    // The end-to-end assertion: Bob receives + decrypts Alice's plaintext via
    // the mailbox-fallback path. Generous deadline (trigger ~1 s + a few poll
    // ticks at the fast cadence; the 15 s sweeper is NOT on the critical path).
    wait_for_message(
        &mut bob_sub,
        alice_pub,
        "offline-hello",
        Duration::from_secs(45),
    )
    .await;

    // --- Graceful shutdown ---
    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
