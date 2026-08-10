// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #118 acceptance: `Command::SaveAttachment` produces a byte-identical
//! decrypt of a completed inbound attachment, and refuses (leaving no
//! partial file) when the transfer has not completed yet.
//!
//! Reuses the `attachment_transfer_direct` harness verbatim (two real
//! `run_with_transport` daemons over `LoopbackTransport`, first contact via
//! invite, then `SendFile`) — only the assertions after the transfer differ.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use sha2::{Digest, Sha256};
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, IpcClient, Ready};
use skattr_core::test_exports::{run_loopback, LoopbackNet};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::loopback_harness::{config_for, init_vault, wait_for_group_active, PASSPHRASE};

fn deterministic_payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[allow(clippy::type_complexity)]
fn spawn_daemon(
    dir: &std::path::Path,
    onion: &str,
    net: LoopbackNet,
    pw: Zeroizing<String>,
    download_dir: std::path::PathBuf,
) -> (
    oneshot::Receiver<Ready>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<skattr_core::Result<()>>,
) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let dir = dir.to_path_buf();
    let mut cfg = config_for(&dir);
    cfg.download_dir = Some(download_dir);
    let onion = onion.to_string();
    let task = tokio::spawn(async move {
        run_loopback(
            &dir,
            &pw,
            cfg,
            std::path::PathBuf::from("/dev/null"),
            net,
            onion,
            ready_tx,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    (ready_rx, shutdown_tx, task)
}

async fn wait_for_attachment(
    sub: &mut IpcClient<skattr_core::daemon::ipc::IpcStream>,
    sender: skattr_core::identity::PublicKey,
    want_id: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "AttachmentReceived(id={want_id}) from {sender:?} not seen in {timeout:?}"
        );
        match tokio::time::timeout(remaining, sub.next_event()).await {
            Ok(Ok(Event::AttachmentReceived {
                contact,
                attachment_id,
                ..
            })) if contact == sender && attachment_id.to_string() == want_id => {
                return;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("subscribe stream error: {e:?}"),
            Err(_) => {
                panic!("AttachmentReceived(id={want_id}) from {sender:?} not seen in {timeout:?}")
            }
        }
    }
}

/// Byte-identical round-trip: a completed inbound attachment saved via
/// `Command::SaveAttachment` must equal the original file exactly, and their
/// sha256 digests must match (the manifest's integrity guarantee).
///
/// A second case in the same test: saving an attachment that is still
/// `pending` (Alice has queued it but Bob has not yet received all chunks)
/// must return an error, and must not create the destination file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn save_attachment_is_byte_identical_and_rejects_pending() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let net = LoopbackNet::new();
    let pw = Zeroizing::new(PASSPHRASE.to_string());

    let bob_download_dir = tmp_b.path().join("downloads");

    let (ready_a_rx, shutdown_a_tx, task_a) = spawn_daemon(
        tmp_a.path(),
        "alice.onion",
        net.clone(),
        pw.clone(),
        tmp_a.path().join("downloads"),
    );
    let (ready_b_rx, shutdown_b_tx, task_b) = spawn_daemon(
        tmp_b.path(),
        "bob.onion",
        net.clone(),
        pw.clone(),
        bob_download_dir.clone(),
    );

    let ready_a: Ready = tokio::time::timeout(Duration::from_secs(60), ready_a_rx)
        .await
        .expect("Alice ready within 60 s")
        .expect("Alice ready_tx open");
    let ready_b: Ready = tokio::time::timeout(Duration::from_secs(60), ready_b_rx)
        .await
        .expect("Bob ready within 60 s")
        .expect("Bob ready_tx open");

    // --- First contact ---
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

    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let alice_pubkey = match client_b
        .execute(Command::AddContact {
            invite_url: invite_url.clone(),
        })
        .await
        .unwrap()
    {
        CommandResult::ContactAdded(s) => s.pubkey,
        other => panic!("expected ContactAdded, got {other:?}"),
    };

    let mut client_b_info = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let bob_pubkey = match client_b_info.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };

    wait_for_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(30)).await;

    let mut bob_sub = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    bob_sub.subscribe(EventFilter::All).await.unwrap();

    // === Send a multi-chunk file and let it complete ===
    let payload = deterministic_payload(700 * 1024);
    let src = tmp_a.path().join("payload.bin");
    std::fs::write(&src, &payload).unwrap();

    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let attachment_id = match send_a
        .execute(Command::SendFile {
            contact: bob_pubkey,
            path: src.to_string_lossy().to_string(),
        })
        .await
        .unwrap()
    {
        CommandResult::FileQueued {
            attachment_id,
            total_chunks,
            ..
        } => {
            assert!(
                total_chunks >= 3,
                "expected ≥3 chunks for a 700 KiB payload, got {total_chunks}"
            );
            attachment_id.to_string()
        }
        other => panic!("expected FileQueued, got {other:?}"),
    };

    wait_for_attachment(
        &mut bob_sub,
        alice_pubkey,
        &attachment_id,
        Duration::from_secs(60),
    )
    .await;

    // --- Positive case: completed attachment saves byte-identical ---
    let out = tmp_b.path().join("saved-payload.bin");
    let aid: skattr_core::daemon::hex::Hex16 = attachment_id.parse().unwrap();
    let mut bob_save = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    match bob_save
        .execute(Command::SaveAttachment {
            attachment_id: aid,
            dest_path: out.to_string_lossy().to_string(),
        })
        .await
        .unwrap()
    {
        CommandResult::Ok => {}
        other => panic!("expected Ok from SaveAttachment, got {other:?}"),
    }
    let saved = std::fs::read(&out).unwrap();
    assert_eq!(
        saved, payload,
        "decrypted attachment must be byte-identical to the source"
    );
    let saved_hash = Sha256::digest(&saved);
    let src_hash = Sha256::digest(&payload);
    assert_eq!(
        saved_hash, src_hash,
        "sha256 of the saved attachment must match the source"
    );

    // --- Negative case: saving a still-pending attachment errors, no partial file ---
    // Queue a second, larger file on Alice's side but do NOT wait for Bob to
    // receive it — race SaveAttachment against the transfer so the row is
    // still 'pending' (direction='in', status != 'complete') at save time.
    let pending_payload = deterministic_payload(5 * 1024 * 1024);
    let pending_src = tmp_a.path().join("pending.bin");
    std::fs::write(&pending_src, &pending_payload).unwrap();

    let mut send_a2 = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let pending_attachment_id = match send_a2
        .execute(Command::SendFile {
            contact: bob_pubkey,
            path: pending_src.to_string_lossy().to_string(),
        })
        .await
        .unwrap()
    {
        CommandResult::FileQueued { attachment_id, .. } => attachment_id.to_string(),
        other => panic!("expected FileQueued, got {other:?}"),
    };

    let pending_out = tmp_b.path().join("pending-out.bin");
    let pending_aid: skattr_core::daemon::hex::Hex16 = pending_attachment_id.parse().unwrap();
    let mut bob_save_pending = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let result = bob_save_pending
        .execute(Command::SaveAttachment {
            attachment_id: pending_aid,
            dest_path: pending_out.to_string_lossy().to_string(),
        })
        .await;
    assert!(
        result.is_err() || !matches!(result, Ok(CommandResult::Ok)),
        "SaveAttachment on a pending attachment must not report Ok, got {result:?}"
    );
    assert!(
        !pending_out.exists(),
        "SaveAttachment must not create a partial file for a pending attachment"
    );

    // --- Graceful shutdown ---
    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
