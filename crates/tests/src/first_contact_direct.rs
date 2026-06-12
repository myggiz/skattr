// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 1C exit-criterion guardrail: two real `run_with_transport` daemon
//! assemblies complete the **full first-contact flow** over an in-process
//! `LoopbackTransport` — no Tor, no seeding, no `test_exports` hub
//! hand-wiring.
//!
//! Unlike the Phase 1B guardrail (`daemon_run_direct.rs`), which seeds two
//! daemons as *already-established* contacts via `seed_established_pair`,
//! this test drives the REAL invite path end-to-end:
//!
//!   1. Alice `Command::CreateInvite` — the invite embeds Alice's signed
//!      self-`ContactCard` carrying her loopback onion (ADR 0008 / Task 1).
//!   2. Bob `Command::AddContact(url)` — Bob verifies + persists Alice's card
//!      (Task 2), dials Alice's onion to deliver the MLS Welcome (Task 4's
//!      dial-on-demand Welcome arm), and sends Bob's own self-card back so
//!      Alice can route the reverse direction (Task 3).
//!   3. Alice's group transitions `PendingJoin → Active` once the Welcome
//!      lands via the dial — proving Task 4.
//!   4. BIDIRECTIONAL messages: Alice→Bob (Bob already knows Alice's onion
//!      from the embedded card) AND Bob→Alice (Alice learned Bob's onion from
//!      his card-send in step 2).
//!
//! Everything runs through the production `run_with_transport` assembly
//! (outbound dialer + inbound accept loop + DaemonInbound MLS decrypt + IPC)
//! via `test_exports::run_loopback`, which only swaps Arti for an in-process
//! `LoopbackTransport`. This is the live CI guardrail that proves all of
//! Phase 1C Tasks 1–4 compose into a working first-contact flow.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, Config, IpcClient, Ready};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::{run_loopback, LoopbackNet};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const PASSPHRASE: &str = "first-contact-guardrail-passphrase-xyz";

// ---------------------------------------------------------------------------
// Daemon-spawn scaffolding (copied from `daemon_run_direct.rs`)
// ---------------------------------------------------------------------------

/// Initialise a fresh identity vault at `data_dir/identity.vault`.
fn init_vault(data_dir: &Path) {
    std::fs::create_dir_all(data_dir).unwrap();
    let seed = skattr_core::identity::Seed::generate().unwrap();
    let identity = skattr_core::identity::IdentityKey::from_seed(&seed).unwrap();
    skattr_core::identity::Vault::create(&data_dir.join("identity.vault"), identity, PASSPHRASE)
        .unwrap();
}

/// Build a `Config` with a unique data dir + IPC socket path under `data_dir`.
fn config_for(data_dir: &Path) -> Config {
    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.to_path_buf();
    config.ipc_socket = Some(data_dir.join("daemon.sock"));
    config
}

/// Poll `ipc_path`'s `ListContacts` until the entry for `peer` reports
/// `group_state == Active`, or panic after `timeout`.
async fn wait_for_group_active(ipc_path: &Path, peer: PublicKey, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut client = IpcClient::connect(ipc_path).await.expect("connect IPC");
        if let CommandResult::Contacts(v) = client
            .execute(Command::ListContacts)
            .await
            .expect("ListContacts")
        {
            if let Some(s) = v.into_iter().find(|s| s.pubkey == peer) {
                if s.group_state == Some(MlsGroupStateLabel::Active) {
                    return;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("group_state for {peer:?} did not become Active within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Open a subscription on `ipc_path` for `MessageReceived` events from
/// `sender`. Must be established **before** the message is sent so the event
/// cannot fire before we are listening.
async fn subscribe_messages(
    ipc_path: &Path,
    sender: PublicKey,
) -> IpcClient<skattr_core::daemon::ipc::IpcStream> {
    let mut sub = IpcClient::connect(ipc_path)
        .await
        .expect("connect for subscribe");
    sub.subscribe(EventFilter::Messages {
        contact: Some(sender),
    })
    .await
    .expect("subscribe to Messages");
    sub
}

/// Drain a pre-established subscription until a `MessageReceived` from
/// `sender` whose body equals `expected_body` arrives, or panic after
/// `timeout`.
async fn wait_for_message(
    sub: &mut IpcClient<skattr_core::daemon::ipc::IpcStream>,
    sender: PublicKey,
    expected_body: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "MessageReceived(body={expected_body:?}) from {sender:?} not seen in {timeout:?}"
            );
        }
        match tokio::time::timeout(remaining, sub.next_event()).await {
            Ok(Ok(Event::MessageReceived { contact, record })) if contact == sender => {
                if let Kind::Text { body } = &record.kind {
                    if body == expected_body {
                        return;
                    }
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("subscribe stream error: {e:?}"),
            Err(_) => panic!(
                "MessageReceived(body={expected_body:?}) from {sender:?} not seen in {timeout:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Fast loopback first-contact guardrail (NOT #[ignore])
// ---------------------------------------------------------------------------

/// Two real `run_with_transport` daemons over `LoopbackTransport` complete the
/// REAL first-contact flow (invite → add → Welcome-via-dial → bidirectional
/// messages) with no Tor, no seeding, and no `test_exports` hub hand-wiring.
/// This is the Phase 1C exit criterion — it exercises Tasks 1–4 together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_contact_invite_add_then_bidirectional_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    // Shared in-process net; each daemon publishes a distinct onion. The onion
    // handed to `run_loopback` is exactly what the daemon advertises (and what
    // the invite/ContactCard carries), so it is the registry key the dialer
    // resolves against.
    let net = LoopbackNet::new();
    let pw = Zeroizing::new(PASSPHRASE.to_string());

    // --- Spawn Alice ---
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

    // --- Spawn Bob ---
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

    let ready_a: Ready = tokio::time::timeout(Duration::from_secs(60), ready_a_rx)
        .await
        .expect("Alice ready within 60 s")
        .expect("Alice ready_tx open");
    let ready_b: Ready = tokio::time::timeout(Duration::from_secs(60), ready_b_rx)
        .await
        .expect("Bob ready within 60 s")
        .expect("Bob ready_tx open");

    assert_eq!(
        ready_a.onion, "alice.onion",
        "advertised onion must match registry key"
    );
    assert_eq!(
        ready_b.onion, "bob.onion",
        "advertised onion must match registry key"
    );

    // --- Step 2: Alice creates an invite (embeds her self-card / onion) ---
    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let invite_url = match tokio::time::timeout(
        Duration::from_secs(30),
        client_a.execute(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(600),
        }),
    )
    .await
    .expect("CreateInvite completes within 30 s")
    .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };
    assert!(
        invite_url.starts_with("skattr://invite/v1#"),
        "invite URL must use canonical scheme"
    );

    // --- Step 3: Bob adds the invite. This persists Alice's card, dials her
    // onion to deliver the Welcome, and sends Bob's card back to Alice. ---
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let alice_summary = match tokio::time::timeout(
        Duration::from_secs(30),
        client_b.execute(Command::AddContact {
            invite_url: invite_url.clone(),
        }),
    )
    .await
    .expect("AddContact completes within 30 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    let alice_pubkey = alice_summary.pubkey;

    // Bob's pubkey, for Alice to look up when sending the reverse direction.
    let mut client_b_info = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let bob_pubkey = match client_b_info.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };

    // --- Step 4: Alice's group must reach Active (the Welcome landed via the
    // on-demand dial — Task 4). ---
    wait_for_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(30)).await;

    // --- Step 5: Alice → Bob. Subscribe BEFORE sending (delivery can complete
    // synchronously inside SendMessage). ---
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

    // --- Step 6: Bob → Alice. Requires Alice to have learned Bob's onion from
    // his card-send in step 3 (Task 3). ---
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

    // --- Step 7: Graceful shutdown ---
    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
