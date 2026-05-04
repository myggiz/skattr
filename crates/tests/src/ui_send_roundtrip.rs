// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! End-to-end UI-style message roundtrip over real Arti.
//!
//! Mirrors `cli_real_tor.rs` but asserts the Phase 2.D wire-format
//! additions:
//!
//! - `MessageSent.record.is_some()` on the sender side
//! - `MessageRecord.row_id > 0` and `direction == Outgoing` on the
//!   sender-side row surfaced via `Command::RecentMessages`
//! - `ContactSummary.last_read_row_id` reflects the cursor after
//!   `Command::MarkRead`
//!
//! # Note on `MessageReceived` assertion
//!
//! The current IPC invite flow (`AddContact`) creates an MLS group only
//! on the *consumer* side (Bob) — Alice has no group state and cannot
//! decrypt Bob's ciphertext.  Real `Event::MessageReceived` delivery to
//! Alice therefore cannot be asserted here without a symmetric Welcome
//! exchange (tracked as a future workstream).  Instead, we verify the
//! Phase 2.D sender-side record fields and the post-`MarkRead` cursor.
//!
//! Once the symmetric Welcome path lands, this test can be extended to
//! subscribe to Bob's event stream and assert `MessageReceived.record.row_id`.
//!
//! # Running
//!
//! ```bash
//! cargo test -p skattr-tests --features test-harness --release \
//!     ui_send_roundtrip -- --ignored
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// TODO(2.D-followup): refactor shared with cli_real_tor.rs — the
// `spawn_real_daemon` helper below is a copy; consider extracting it
// to a `crates/tests/src/real_tor_harness.rs` module once both tests
// stabilise.

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::commands::{Direction, SendStatus};
use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, Ready};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------
// Spawn helper (copied from cli_real_tor.rs — see TODO above)
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
    let pw = Zeroizing::new("ui-roundtrip-passphrase-xyz".to_string());
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
            std::path::PathBuf::from("/dev/null"),
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
// Test
// ---------------------------------------------------------------------------

/// Full invite → add → send flow over two real Arti daemons, asserting
/// Phase 2.D wire-format additions.
///
/// Both daemons bootstrap independently; each publishes its own onion
/// service.  The test then:
///
/// 1. Alice creates an invite.
/// 2. Bob adds Alice.
/// 3. Bob sends to Alice and checks `MessageSent.record.is_some()`,
///    `record.row_id > 0`, and `record.direction == Outgoing`.
/// 4. Bob retrieves the first page of `RecentMessages` and confirms the
///    same row appears with the Phase 2.D `row_id` field populated.
/// 5. Bob calls `MarkRead` then `ListContacts` and confirms
///    `ContactSummary.last_read_row_id` reflects the cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Tor bootstrap; run with: cargo test -p skattr-tests --features test-harness --release ui_send_roundtrip -- --ignored"]
async fn ui_send_roundtrip_over_real_tor() {
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

    // --- Step 1: Alice creates an invite ---
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

    // --- Step 2: Bob adds Alice ---
    let alice_summary = match tokio::time::timeout(
        Duration::from_secs(60),
        client_b.execute(Command::AddContact { invite_url }),
    )
    .await
    .expect("AddContact must complete within 60 s")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    eprintln!("Bob added Alice: pubkey={:?}", alice_summary.pubkey);

    // --- Step 3: Bob sends to Alice — assert Phase 2.D MessageSent.record ---
    //
    // Accept Queued OR Delivered: over real Tor, circuit RTT may cause the
    // inline hub wait to expire before the ACK arrives (Queued). Either
    // way the record field must be present with a valid row_id.
    let alice_pubkey = alice_summary.pubkey;
    let send_result = client_b
        .execute(Command::SendMessage {
            contact: alice_pubkey,
            kind: skattr_core::envelope::Kind::Text {
                body: "hello-ui-roundtrip".into(),
            },
        })
        .await
        .unwrap();

    let (sender_row_id, send_message_id) = match send_result {
        CommandResult::MessageSent {
            message_id,
            status: SendStatus::Queued,
            record,
        }
        | CommandResult::MessageSent {
            message_id,
            status: SendStatus::Delivered,
            record,
        } => {
            let rec = record.expect("Phase 2.D: MessageSent must carry sender-side record");
            assert!(
                rec.row_id > 0,
                "Phase 2.D: row_id must be > 0, got {}",
                rec.row_id
            );
            assert_eq!(
                rec.direction,
                Direction::Outgoing,
                "Phase 2.D: sender-side record must have direction=Outgoing"
            );
            match &rec.kind {
                skattr_core::envelope::Kind::Text { body } => {
                    assert_eq!(body, "hello-ui-roundtrip");
                }
                other => panic!("unexpected kind {other:?}"),
            }
            eprintln!(
                "Phase 2.D MessageSent.record validated: row_id={} direction={:?}",
                rec.row_id, rec.direction
            );
            (rec.row_id, message_id)
        }
        other => panic!("expected MessageSent, got {other:?}"),
    };

    // --- Step 4: RecentMessages — verify the sender-side row is persisted ---
    //
    // A fresh IpcClient is needed because `execute` is one-shot per connection.
    let mut client_b2 = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let recent = match client_b2
        .execute(Command::RecentMessages {
            contact: Some(alice_pubkey),
            limit: 10,
            before_id: None,
            paged: false,
        })
        .await
        .unwrap()
    {
        CommandResult::Messages(rows) => rows,
        other => panic!("expected Messages, got {other:?}"),
    };

    let found = recent
        .iter()
        .find(|r| r.row_id == sender_row_id)
        .unwrap_or_else(|| {
            panic!(
                "Phase 2.D: sender row_id={sender_row_id} not found in RecentMessages; \
                 got: {recent:?}"
            )
        });
    assert_eq!(
        found.direction,
        Direction::Outgoing,
        "Phase 2.D: persisted row must have direction=Outgoing"
    );
    eprintln!(
        "Phase 2.D RecentMessages row validated: row_id={}",
        found.row_id
    );

    // --- Step 5: MarkRead → ListContacts → assert last_read_row_id cursor ---
    let mut client_b3 = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    match client_b3
        .execute(Command::MarkRead {
            contact: alice_pubkey,
            up_to_message_id: sender_row_id,
        })
        .await
        .unwrap()
    {
        CommandResult::MarkedRead { up_to } => {
            assert_eq!(
                up_to, sender_row_id,
                "MarkRead must echo back the cursor row_id"
            );
        }
        other => panic!("expected MarkedRead, got {other:?}"),
    }
    eprintln!("MarkRead confirmed for row_id={sender_row_id}");

    let mut client_b4 = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let summaries = match client_b4.execute(Command::ListContacts).await.unwrap() {
        CommandResult::Contacts(s) => s,
        other => panic!("expected Contacts, got {other:?}"),
    };
    let alice_contact = summaries
        .into_iter()
        .find(|c| c.pubkey == alice_pubkey)
        .unwrap_or_else(|| panic!("Alice not found in Bob's contact list"));

    assert_eq!(
        alice_contact.last_read_row_id,
        Some(sender_row_id),
        "Phase 2.D: ContactSummary.last_read_row_id must reflect post-MarkRead cursor; \
         got: {:?}",
        alice_contact.last_read_row_id
    );
    eprintln!(
        "Phase 2.D ContactSummary.last_read_row_id validated: {:?}",
        alice_contact.last_read_row_id
    );

    // Suppress unused-variable warning for send_message_id (kept for
    // traceability / future assertion of DeliveryStatusChanged event).
    let _ = send_message_id;

    // --- Graceful shutdown ---
    eprintln!("Shutting down daemons…");
    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
    eprintln!("Done.");
}
