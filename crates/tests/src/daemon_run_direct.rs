// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1B regression guardrail: two real daemon assemblies exchange
//! messages in BOTH directions over the production wiring.
//!
//! Unlike `cli_two_daemons.rs` (which hand-wires two `DeliveryHub`s via
//! `test_exports`), this test drives the *full* `run_with_transport`
//! assembly — outbound dialer + inbound accept loop + ingest — through
//! `test_exports::run_loopback`, an exact twin of `Daemon::run_with_sink`
//! that swaps Arti for an in-process `LoopbackTransport` (no Tor) and a
//! no-op mailbox factory. Everything else (DaemonInbound MLS decrypt,
//! the on-demand dialer, the accept loop, IPC) is the same code the real
//! daemon runs — no `test_exports` hand-wiring of the hub.
//!
//! ## Why seed *established* contacts instead of running invite→add
//!
//! Phase 1B delivers the direct-transport assembly. The full first-contact
//! flow (invite → AddContact → Welcome propagation → first message) needs
//! more first-contact plumbing — Welcome-arm dial-on-demand, inviter-onion
//! bootstrapping, card exchange — that is deferred to **Phase 1C** (ADR
//! 0007 is its down-payment). So this guardrail does NOT exercise the
//! invite/Welcome path. Instead `seed_established_pair` writes two daemons'
//! pools as ALREADY-ESTABLISHED contacts (a shared 2-member MLS group +
//! each other's `ContactCard` carrying the loopback onion) before either
//! daemon boots, then proves BIDIRECTIONAL DIRECT message delivery through
//! the real `run_with_transport` assembly.
//!
//! The fast loopback test is NOT `#[ignore]` — it is CI's guardrail. A
//! `#[ignore]` real-Tor twin runs the identical seed-then-exchange script
//! via the public `Daemon::run`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use crate::loopback_harness::{
    config_for, init_vault, subscribe_messages, wait_for_group_active, wait_for_message, PASSPHRASE,
};
use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, Ready};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::{run_loopback, seed_established_pair, LoopbackNet};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// Drive a bidirectional send/receive between two already-running,
/// already-ESTABLISHED daemons (identified by their `Ready`). Assumes
/// `seed_established_pair` has already linked them as mutual contacts with a
/// shared 2-member group and loopback-onion ContactCards. Shared by both the
/// loopback and real-Tor twins.
async fn run_exchange_script(ready_a: &Ready, ready_b: &Ready) {
    // Discover each side's pubkey.
    let mut info_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let alice_pubkey = match info_a.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };
    let mut info_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let bob_pubkey = match info_b.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };

    // Both groups are seeded Active. Sanity-check before driving messages so a
    // seeding regression fails loudly here rather than as a delivery timeout.
    wait_for_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(10)).await;
    wait_for_group_active(&ready_b.ipc_socket, alice_pubkey, Duration::from_secs(10)).await;

    // --- Alice → Bob ---
    // Subscribe BEFORE sending: the direct dial+deliver can complete inside the
    // `SendMessage` call, so a post-send subscribe could miss the event.
    let mut bob_sub = subscribe_messages(&ready_b.ipc_socket, alice_pubkey).await;
    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(30),
        send_a.execute(Command::SendMessage {
            contact: bob_pubkey,
            kind: Kind::Text {
                body: "hello-bob".into(),
            },
        }),
    )
    .await
    .expect("Alice send completes within 30 s")
    .unwrap()
    {
        CommandResult::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {other:?}"),
    }
    wait_for_message(
        &mut bob_sub,
        alice_pubkey,
        "hello-bob",
        Duration::from_secs(30),
    )
    .await;

    // --- Bob → Alice ---
    let mut alice_sub = subscribe_messages(&ready_a.ipc_socket, bob_pubkey).await;
    let mut send_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(30),
        send_b.execute(Command::SendMessage {
            contact: alice_pubkey,
            kind: Kind::Text {
                body: "hello-alice".into(),
            },
        }),
    )
    .await
    .expect("Bob send completes within 30 s")
    .unwrap()
    {
        CommandResult::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {other:?}"),
    }
    wait_for_message(
        &mut alice_sub,
        bob_pubkey,
        "hello-alice",
        Duration::from_secs(30),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Fast loopback guardrail (NOT #[ignore])
// ---------------------------------------------------------------------------

/// Two real daemon assemblies over `LoopbackTransport`, seeded as
/// already-established contacts, exchange messages in both directions —
/// driving the production `run_with_transport` wiring (outbound dialer +
/// inbound accept loop + ingest) with no Tor and no `test_exports`
/// hand-wiring of the hub. This is Phase 1B's live CI guardrail against the
/// audit's "dead transport" gap.
///
/// `seed_established_pair` writes both pools as mutual contacts sharing a
/// real 2-member MLS group, each carrying the other's loopback-onion
/// `ContactCard`, BEFORE either daemon boots. The full first-contact
/// invite → add → Welcome → first-message flow is deliberately NOT exercised
/// here — it is deferred to Phase 1C (ADR 0007 is its down-payment).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_daemons_exchange_messages_both_directions_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    // Seed BOTH daemons as established contacts BEFORE either opens its pool.
    // `seed_established_pair` opens each pool the same way `run_loopback`
    // will, writes the shared group + mutual contacts + loopback-onion cards,
    // then drops the pools so the daemons can re-open them.
    let pw_seed = Zeroizing::new(PASSPHRASE.to_string());
    seed_established_pair(
        tmp_a.path(),
        tmp_b.path(),
        &pw_seed,
        "alice.onion",
        "bob.onion",
    )
    .expect("seed established pair");

    // Shared in-process net; each daemon publishes a distinct onion. The
    // onion handed to `LoopbackTransport::new` is exactly what
    // `transport.publish` returns and what the daemon advertises via
    // `handle.set_onion`, so each side's ContactCard/invite carries the
    // SAME string that is the registry key the dialer resolves against.
    let net = LoopbackNet::new();

    let pw = Zeroizing::new(PASSPHRASE.to_string());

    // --- Alice ---
    let (ready_a_tx, ready_a_rx) = oneshot::channel();
    let (shutdown_a_tx, shutdown_a_rx) = oneshot::channel::<()>();
    let a_dir = tmp_a.path().to_path_buf();
    let a_cfg = config_for(tmp_a.path());
    let a_net = net.clone();
    let a_pw = pw.clone();
    let task_a = tokio::spawn(async move {
        run_loopback(
            &a_dir,
            &a_pw,
            a_cfg,
            std::path::PathBuf::from("/dev/null"),
            a_net,
            "alice.onion".into(),
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
    let b_cfg = config_for(tmp_b.path());
    let b_net = net.clone();
    let b_pw = pw.clone();
    let task_b = tokio::spawn(async move {
        run_loopback(
            &b_dir,
            &b_pw,
            b_cfg,
            std::path::PathBuf::from("/dev/null"),
            b_net,
            "bob.onion".into(),
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

    assert_eq!(
        ready_a.onion, "alice.onion",
        "advertised onion must match registry key"
    );
    assert_eq!(
        ready_b.onion, "bob.onion",
        "advertised onion must match registry key"
    );

    run_exchange_script(&ready_a, &ready_b).await;

    // --- Graceful shutdown ---
    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}

// ---------------------------------------------------------------------------
// Real-Tor twin (#[ignore])
// ---------------------------------------------------------------------------

/// Spin up a real daemon at `data_dir` with Arti bootstrap (mirrors
/// `cli_real_tor.rs`). The vault must already exist (seeded by the caller
/// via `init_vault` + `seed_established_pair` so the two daemons boot as
/// established contacts).
async fn spawn_real_daemon(
    data_dir: &Path,
) -> (
    Ready,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<skattr_core::error::Result<()>>,
) {
    let config = config_for(data_dir);
    let pw = Zeroizing::new(PASSPHRASE.to_string());

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let data_dir_owned = data_dir.to_path_buf();
    let task = tokio::spawn(async move {
        Daemon::run(
            &data_dir_owned,
            &pw,
            config,
            std::path::PathBuf::from("/dev/null"),
            ready_tx,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    let ready = tokio::time::timeout(Duration::from_secs(180), ready_rx)
        .await
        .expect("daemon bootstraps within 180 s")
        .expect("ready_tx still open");

    (ready, shutdown_tx, task)
}

/// Same seeded-established-contacts bidirectional-send script as the loopback
/// guardrail, but over two real Arti daemons via the public `Daemon::run`.
///
/// Real onion addresses are derived from each daemon's HS key at publish
/// time, so they are not known until `Ready`. The seeding therefore runs in
/// two phases: the vaults are created, the daemons boot to learn their real
/// onions, and `seed_established_pair` then writes the shared group + mutual
/// contacts + real-onion ContactCards. Live pickup of contacts written while
/// the daemon already holds the pool is itself first-contact plumbing
/// deferred to Phase 1C, which is why this twin stays `#[ignore]`-gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Tor; run with: cargo test -p skattr-tests --release -- --ignored two_daemons_exchange_messages_over_real_tor"]
async fn two_daemons_exchange_messages_over_real_tor() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();

    // Phase 1: create vaults, boot both daemons to learn their real onions.
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());
    let (ready_a, shutdown_a, task_a) = spawn_real_daemon(tmp_a.path()).await;
    let (ready_b, shutdown_b, task_b) = spawn_real_daemon(tmp_b.path()).await;

    // Phase 2: seed established contacts with the real published onions.
    let pw_seed = Zeroizing::new(PASSPHRASE.to_string());
    seed_established_pair(
        tmp_a.path(),
        tmp_b.path(),
        &pw_seed,
        &ready_a.onion,
        &ready_b.onion,
    )
    .expect("seed established pair");

    run_exchange_script(&ready_a, &ready_b).await;

    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
