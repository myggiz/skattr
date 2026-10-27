// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! SKA-94 guardrail: when one daemon restarts, the other must still be able
//! to send to it, with no restart of its own.
//!
//! ## The failure this exists to catch
//!
//! A daemon closes its connections on shutdown, and a graceful close sends
//! `Frame::Bye`. The receiving peer actor used to treat `Bye` as "stop the
//! actor", while the hub kept handing out that actor's channels. Every later
//! send to the restarted contact went into a dead channel and was never
//! retried, and its dial-ins were dropped too. The contact went silent until
//! the surviving daemon restarted as well.
//!
//! ## Shape of the test
//!
//! Alice and Bob both run, and Alice sends Bob a message so a live connection
//! exists between them. Alice restarts on the same data dir. Bob then sends to
//! Alice, and Alice must receive it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::{seed_established_pair, LoopbackNet};
use zeroize::Zeroizing;

use crate::loopback_harness::{init_vault, PASSPHRASE};
use crate::queued_delivery_on_peer_return::{recent, spawn};

const ALICE_ONION: &str = "alice-peer-restart.onion";
const BOB_ONION: &str = "bob-peer-restart.onion";

const BEFORE: &str = "before alice restarts";
const AFTER: &str = "after alice restarted";

async fn local_pubkey(ipc: &Path) -> PublicKey {
    let mut client = IpcClient::connect(ipc).await.unwrap();
    match client.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    }
}

async fn send(ipc: &Path, to: PublicKey, body: &str) {
    let mut client = IpcClient::connect(ipc).await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(30),
        client.execute(Command::SendMessage {
            contact: to,
            kind: Kind::Text { body: body.into() },
        }),
    )
    .await
    .expect("SendMessage returns within 30 s")
    .unwrap()
    {
        CommandResult::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {other:?}"),
    }
}

async fn wait_until_received(ipc: &Path, from: PublicKey, body: &str, secs: u64, why: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let got = recent(ipc, from)
            .await
            .into_iter()
            .any(|r| matches!(&r.kind, Kind::Text { body: b } if b == body));
        if got {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{why}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_can_still_send_after_the_other_daemon_restarts_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let pw_seed = Zeroizing::new(PASSPHRASE.to_string());
    seed_established_pair(tmp_a.path(), tmp_b.path(), &pw_seed, ALICE_ONION, BOB_ONION)
        .expect("seed established pair");

    let net = LoopbackNet::new();
    let (ready_b, shutdown_b, task_b) = spawn(tmp_b.path(), &net, BOB_ONION).await;
    let (ready_a, shutdown_a, task_a) = spawn(tmp_a.path(), &net, ALICE_ONION).await;

    let alice_pubkey = local_pubkey(&ready_a.ipc_socket).await;
    let bob_pubkey = local_pubkey(&ready_b.ipc_socket).await;

    // A live connection between them, so Alice's shutdown has one to close.
    send(&ready_a.ipc_socket, bob_pubkey, BEFORE).await;
    wait_until_received(
        &ready_b.ipc_socket,
        alice_pubkey,
        BEFORE,
        60,
        "precondition broken: Bob never received Alice's first message",
    )
    .await;

    // Alice restarts. Her shutdown closes the connection with a Bye.
    let _ = shutdown_a.send(());
    tokio::time::timeout(Duration::from_secs(30), task_a)
        .await
        .expect("Alice shuts down within 30 s")
        .unwrap()
        .expect("Alice's first run exits cleanly");
    let (ready_a, shutdown_a, task_a) = spawn(tmp_a.path(), &net, ALICE_ONION).await;

    send(&ready_b.ipc_socket, alice_pubkey, AFTER).await;
    wait_until_received(
        &ready_a.ipc_socket,
        bob_pubkey,
        AFTER,
        120,
        "SKA-94: Bob's message never reached Alice after she restarted. Bob's \
         actor for Alice stopped on her shutdown Bye, so his send went into a \
         dead channel and nothing retried it.",
    )
    .await;

    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
