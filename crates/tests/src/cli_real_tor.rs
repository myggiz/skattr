// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Two-daemon E2E over real Arti. Ignored by default — run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored cli_real_tor
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, Ready};
use skattr_core::envelope::Kind;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------
// Spawn helper
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

    // Generate a fresh identity and persist it to the vault.
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    let pw = Zeroizing::new("real-tor-passphrase-xyz".to_string());
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

    // Move owned values into the 'static async block.
    let data_dir_owned = data_dir.to_path_buf();
    let pw_owned = pw.clone();
    let config_owned = config.clone();
    let task = tokio::spawn(async move {
        Daemon::run(
            &data_dir_owned,
            &pw_owned,
            config_owned,
            std::path::PathBuf::from("/dev/null"),
            ready_tx,
            shutdown_fut,
        )
        .await
    });

    let ready = tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx)
        .await
        .expect("daemon bootstraps within 180 s")
        .expect("ready_tx still open");

    (ready, shutdown_tx, task)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Full invite → add → send flow over two real Arti daemons.
///
/// Both daemons bootstrap independently; each publishes its own onion service.
/// The send result is accepted as either `Queued` or `Delivered` — over real
/// Tor, circuit RTT may cause the 2 s inline hub wait to expire before the
/// ACK arrives (Queued), but the encrypt + outbox path still succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor bootstrap; run with: cargo test -p skattr-tests --release -- --ignored cli_real_tor"]
async fn full_flow_over_real_tor() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();

    eprintln!("Bootstrapping Alice daemon…");
    let (ready_a, shutdown_a, task_a) = spawn_real_daemon(tmp_a.path()).await;
    eprintln!("Alice ready — onion: {}", ready_a.onion);

    eprintln!("Bootstrapping Bob daemon…");
    let (ready_b, shutdown_b, task_b) = spawn_real_daemon(tmp_b.path()).await;
    eprintln!("Bob ready — onion: {}", ready_b.onion);

    // Connect IPC clients to both daemons.
    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();

    // --- Alice creates an invite ---
    let invite_url = match client_a
        .execute(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
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

    // --- Bob adds Alice ---
    let summary = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        client_b.execute(Command::AddContact { invite_url }),
    )
    .await
    .expect("AddContact must complete within 60 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    eprintln!("Bob added Alice: pubkey={:?}", summary.pubkey);

    // --- Bob sends a message to Alice ---
    //
    // We accept Queued OR Delivered: over real Tor, the hub actor will dial
    // Alice's onion and attempt delivery. If the circuit comes up within
    // the 2 s hub wait, we may get Delivered. Either way, the encrypt +
    // outbox path returned MessageSent, which is the assertion that counts.
    let send_result = client_b
        .execute(Command::SendMessage {
            contact: summary.pubkey,
            kind: Kind::Text {
                body: "hello-over-tor".into(),
            },
        })
        .await
        .unwrap();

    match send_result {
        CommandResult::MessageSent { .. } => {
            eprintln!("MessageSent returned — test passed.");
        }
        other => panic!("expected MessageSent, got {other:?}"),
    }

    // --- Graceful shutdown ---
    eprintln!("Shutting down daemons…");
    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), task_b).await;
    eprintln!("Done.");
}
