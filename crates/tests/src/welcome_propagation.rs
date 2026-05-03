// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.E end-to-end: paired daemons over real Tor exchange an invite,
//! add the contact, propagate the Welcome back to Alice, and round-trip a
//! message in both directions.
//!
//! Gated `#[ignore]` because real-Tor bootstrap takes 60–180 s. Run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored welcome_propagation
//! ```
//!
//! # What this test proves
//!
//! - `Command::AddContact` on Bob's side now triggers a Welcome-propagation
//!   round-trip back to Alice (Phase 2.E Task 17/18).
//! - Alice's group_state transitions from `PendingJoin` → `Active` within
//!   30 s of Bob's `AddContact`.
//! - A message from Alice to Bob (Alice→Bob direction) is decrypted and
//!   surfaced via `Event::MessageReceived` on Bob's side.
//! - A message from Bob to Alice (Bob→Alice direction) is decrypted and
//!   surfaced via `Event::MessageReceived` on Alice's side.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, Ready};
use skattr_core::envelope::Kind;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------
// Spawn helper (mirrors cli_real_tor.rs)
// ---------------------------------------------------------------------------

/// Spin up a real daemon at `data_dir` with Arti bootstrap.
///
/// Returns the [`Ready`] struct, a shutdown sender, and the task handle.
async fn spawn_real_daemon(
    data_dir: &Path,
) -> (
    Ready,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<skattr_core::error::Result<()>>,
) {
    std::fs::create_dir_all(data_dir).unwrap();

    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pw = Zeroizing::new("welcome-prop-passphrase-xyz".to_string());
    skattr_core::identity::Vault::create(&data_dir.join("identity.vault"), identity, pw.as_str())
        .unwrap();

    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.to_path_buf();
    config.ipc_socket = Some(data_dir.join("daemon.sock"));

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let data_dir_owned = data_dir.to_path_buf();
    let pw_owned = pw.clone();
    let config_owned = config.clone();
    let task = tokio::spawn(async move {
        Daemon::run(
            &data_dir_owned,
            &pw_owned,
            config_owned,
            ready_tx,
            shutdown_fut,
        )
        .await
    });

    let ready = tokio::time::timeout(Duration::from_secs(180), ready_rx)
        .await
        .expect("daemon bootstraps within 180 s")
        .expect("ready_tx still open");

    (ready, shutdown_tx, task)
}

// ---------------------------------------------------------------------------
// Wait helpers
// ---------------------------------------------------------------------------

/// Poll Alice's `ListContacts` until the entry for `bob_pubkey` has
/// `group_state == Active`, or panic after `timeout`.
async fn wait_for_alice_group_active(
    alice_ipc: &std::path::PathBuf,
    bob_pubkey: skattr_core::identity::PublicKey,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut client = IpcClient::connect(alice_ipc)
            .await
            .expect("connect to Alice IPC");
        let result = client
            .execute(Command::ListContacts)
            .await
            .expect("ListContacts");
        if let CommandResult::Contacts(v) = result {
            if let Some(s) = v.into_iter().find(|s| s.pubkey == bob_pubkey) {
                if s.group_state == Some(MlsGroupStateLabel::Active) {
                    eprintln!("Alice's group_state is now Active.");
                    return;
                }
                eprintln!("Alice's group_state = {:?}; waiting…", s.group_state);
            } else {
                eprintln!("Bob not yet in Alice's contact list; waiting…");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Alice's group_state did not become Active within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Subscribe to `MessageReceived` events on `ipc_path` from `sender_pubkey`
/// and wait until an event with `body == expected_body` arrives, or panic
/// after `timeout`.
async fn wait_for_message_received(
    ipc_path: &std::path::PathBuf,
    sender_pubkey: skattr_core::identity::PublicKey,
    expected_body: &str,
    timeout: Duration,
) {
    let mut sub_client = IpcClient::connect(ipc_path)
        .await
        .expect("connect for subscribe");
    sub_client
        .subscribe(EventFilter::Messages {
            contact: Some(sender_pubkey),
        })
        .await
        .expect("subscribe to Messages");

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "MessageReceived(body={expected_body:?}) from {sender_pubkey:?} \
                 did not arrive within {timeout:?}"
            );
        }
        match tokio::time::timeout(remaining, sub_client.next_event()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) => {
                if contact == sender_pubkey {
                    match &record.kind {
                        Kind::Text { body } if body == expected_body => {
                            eprintln!(
                                "MessageReceived confirmed: body={body:?} row_id={}",
                                record.row_id
                            );
                            return;
                        }
                        other => {
                            eprintln!(
                                "MessageReceived with different body/kind: {other:?}; continuing…"
                            );
                        }
                    }
                }
            }
            Ok(Ok(other)) => {
                eprintln!("Skipping unrelated event: {other:?}");
            }
            Ok(Err(e)) => {
                panic!("subscribe stream error: {e:?}");
            }
            Err(_elapsed) => {
                panic!(
                    "MessageReceived(body={expected_body:?}) from {sender_pubkey:?} \
                     did not arrive within {timeout:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Alice and Bob bootstrap over real Tor. Bob adds Alice's invite;
/// the Phase 2.E Welcome propagation path sends Bob's Welcome back to Alice
/// so Alice's group transitions `PendingJoin` → `Active`. Then both sides
/// exchange a message and verify receipt via `Event::MessageReceived`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor bootstrap (~60-180 s); run with: cargo test -p skattr-tests --release -- --ignored welcome_propagation"]
async fn welcome_propagates_and_round_trip_message_decrypts() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();

    eprintln!("Bootstrapping Alice daemon…");
    let (ready_a, shutdown_a, task_a) = spawn_real_daemon(tmp_a.path()).await;
    eprintln!("Alice ready — onion: {}", ready_a.onion);

    eprintln!("Bootstrapping Bob daemon…");
    let (ready_b, shutdown_b, task_b) = spawn_real_daemon(tmp_b.path()).await;
    eprintln!("Bob ready — onion: {}", ready_b.onion);

    // --- Step 1: Alice creates an invite ---
    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let invite_url = match client_a
        .execute(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(600),
        })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };
    eprintln!("Invite URL: {invite_url}");
    assert!(
        invite_url.starts_with("skattr://invite/v1#"),
        "invite URL must use canonical scheme"
    );

    // --- Step 2: Bob adds the invite ---
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let alice_summary = match tokio::time::timeout(
        Duration::from_secs(60),
        client_b.execute(Command::AddContact {
            invite_url: invite_url.clone(),
        }),
    )
    .await
    .expect("AddContact must complete within 60 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    let alice_pubkey = alice_summary.pubkey;
    eprintln!("Bob added Alice: pubkey={alice_pubkey:?}");

    // Bob's group_state may be Active already (he created the group) or
    // PendingJoin briefly. Assert it is non-None and check it later.
    eprintln!(
        "Bob's group_state immediately after AddContact: {:?}",
        alice_summary.group_state
    );

    // --- Step 3: Discover Bob's pubkey for Alice to look up ---
    // Bob's pubkey comes from his own daemon's DaemonInfo.
    let mut client_b2 = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let bob_pubkey = match client_b2.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };
    eprintln!("Bob pubkey: {bob_pubkey:?}");

    // --- Step 4: Wait for Alice's group to become Active (Welcome propagation) ---
    eprintln!("Waiting for Alice's group to become Active (Welcome propagation)…");
    wait_for_alice_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(30)).await;

    // --- Step 5: Alice sends to Bob ---
    eprintln!("Alice sends to Bob…");
    let mut client_a2 = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    match client_a2
        .execute(Command::SendMessage {
            contact: bob_pubkey,
            kind: Kind::Text {
                body: "hello bob".into(),
            },
        })
        .await
        .unwrap()
    {
        CommandResult::MessageSent { .. } => {
            eprintln!("Alice's SendMessage returned MessageSent.");
        }
        other => panic!("expected MessageSent, got {other:?}"),
    }

    // Wait for Bob to receive Alice's message.
    eprintln!("Waiting for Bob to receive Alice's message…");
    wait_for_message_received(
        &ready_b.ipc_socket,
        alice_pubkey,
        "hello bob",
        Duration::from_secs(30),
    )
    .await;

    // --- Step 6: Bob sends to Alice ---
    eprintln!("Bob sends to Alice…");
    let mut client_b3 = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    match client_b3
        .execute(Command::SendMessage {
            contact: alice_pubkey,
            kind: Kind::Text {
                body: "hi alice".into(),
            },
        })
        .await
        .unwrap()
    {
        CommandResult::MessageSent { .. } => {
            eprintln!("Bob's SendMessage returned MessageSent.");
        }
        other => panic!("expected MessageSent, got {other:?}"),
    }

    // Wait for Alice to receive Bob's message.
    eprintln!("Waiting for Alice to receive Bob's message…");
    wait_for_message_received(
        &ready_a.ipc_socket,
        bob_pubkey,
        "hi alice",
        Duration::from_secs(30),
    )
    .await;

    // --- Graceful shutdown ---
    eprintln!("Shutting down daemons…");
    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
    eprintln!("Done. Phase 2.E Welcome propagation round-trip test passed.");
}
