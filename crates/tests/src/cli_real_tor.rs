// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Two-daemon E2E over real Arti. Ignored by default — run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored cli_real_tor
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::{
    Command, CommandResult, Config, Daemon, IpcClient, IpcClientError, Ready,
};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;
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
// IPC helper — fresh connection per call (IPC is one-shot per connection)
// ---------------------------------------------------------------------------

/// Execute one command against the daemon at `socket_path`.
///
/// The daemon IPC server is one-shot: after each `Execute` it writes the
/// result, writes `Bye`, then closes. This matches how the production CLI
/// works — one OS-level connection per command. A persistent `IpcClient`
/// would see `Io(BrokenPipe)` on the second call.
async fn exec(socket_path: &Path, cmd: Command) -> Result<CommandResult, IpcClientError> {
    IpcClient::connect(socket_path).await?.execute(cmd).await
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

    let sock_a = &ready_a.ipc_socket;
    let sock_b = &ready_b.ipc_socket;

    // --- Alice creates an invite ---
    let invite_url = match exec(
        sock_a,
        Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
        },
    )
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
    //
    // AddContact dials Alice's onion (embedded in the invite's ContactCard)
    // to complete the two-PSK MLS genesis (ADR 0009). This requires live Tor.
    let summary = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        exec(sock_b, Command::AddContact { invite_url }),
    )
    .await
    .expect("AddContact must complete within 60 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    eprintln!("Bob added Alice: pubkey={:?}", summary.pubkey);

    // --- Wait for Bob's group with Alice to reach Active ---
    //
    // Since #93, AddContact leaves the MLS group PendingJoin on Bob's side.
    // SendMessage is gated on the Welcome-Ack completing (group becomes Active).
    // Over real Tor this takes several seconds; poll ListContacts until the
    // group_state flips to Active, bounded at 120 s to distinguish the #90
    // transport flake from a code regression.
    wait_for_active(sock_b, summary.pubkey, std::time::Duration::from_secs(120)).await;

    // --- Bob sends a message to Alice ---
    //
    // We accept Queued OR Delivered: over real Tor, the hub actor will dial
    // Alice's onion and attempt delivery. If the circuit comes up within
    // the 2 s hub wait, we may get Delivered. Either way, the encrypt +
    // outbox path returned MessageSent, which is the assertion that counts.
    let send_result = exec(
        sock_b,
        Command::SendMessage {
            contact: summary.pubkey,
            kind: Kind::Text {
                body: "hello-over-tor".into(),
            },
        },
    )
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

// ---------------------------------------------------------------------------
// Poll helper
// ---------------------------------------------------------------------------

/// Poll `ListContacts` on the daemon at `socket` until the contact identified
/// by `peer` has `group_state == Active`, or until `timeout` elapses.
///
/// Panics with a clear diagnostic if the timeout fires — this distinguishes
/// the #90 transport flake (first contact never completes over real Tor) from
/// a code regression where the group stays PendingJoin indefinitely.
async fn wait_for_active(socket: &Path, peer: PublicKey, timeout: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let contacts = match exec(socket, Command::ListContacts).await.unwrap() {
            CommandResult::Contacts(v) => v,
            other => panic!("expected Contacts, got {other:?}"),
        };
        if let Some(entry) = contacts.iter().find(|s| s.pubkey == peer) {
            if entry.group_state == Some(MlsGroupStateLabel::Active) {
                eprintln!("Group with peer is Active — proceeding to send.");
                return;
            }
            eprintln!(
                "Group state is {:?} — waiting for Active…",
                entry.group_state
            );
        } else {
            eprintln!("Peer not yet in contact list — waiting…");
        }

        if tokio::time::Instant::now() >= deadline {
            panic!(
                "first-contact Welcome-Ack never completed within {:?} — \
                 this is likely the #90 transport flake (DeliveryTimeout over real Tor), \
                 not a code regression. Re-run with live Tor to confirm.",
                timeout
            );
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
