// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #234 guardrail: a message queued before the sender's daemon restarts must
//! still deliver on its own after the restart, with no new user action and no
//! inbound dial from the peer.
//!
//! ## The field failure this exists to catch
//!
//! Six messages queued while the peer was offline. The daemon restarted, then
//! sat for 10.5 hours with zero delivery activity until the peer dialled in.
//! A queued row is only retried by its peer's actor, and nothing spawned one
//! for rows left over from the previous run. `queued_delivery_on_peer_return`
//! could not see it: the sender there never restarts, and its actor exists
//! because the test sent in-process.
//!
//! ## Shape of the test
//!
//! Alice boots alone and sends while Bob is not running, so the message is
//! queued. Alice shuts down. Bob boots, then Alice boots again on the same data
//! dir. From then on nothing acts on the message: no send, no command to Bob,
//! nothing that makes Bob dial. Only Alice's boot-time resume can give her a
//! retry tick for that row. Reading history is passive.
//!
//! Bob is up before Alice reboots, so Alice's first dial after boot should
//! reach him, and the test stays fast. The receive deadline still clears the
//! 15 s and 60 s rungs of `CHUNK_DIAL_BACKOFF_MS`, so one slow first dial
//! cannot make it flaky.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::envelope::Kind;
use skattr_core::test_exports::{seed_established_pair, LoopbackNet};
use zeroize::Zeroizing;

use crate::loopback_harness::{init_vault, PASSPHRASE};
use crate::queued_delivery_on_peer_return::{recent, spawn};

const ALICE_ONION: &str = "alice-restart.onion";
const BOB_ONION: &str = "bob-restart.onion";

const BODY: &str = "queued before the restart";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_delivers_after_sender_restarts_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let pw_seed = Zeroizing::new(PASSPHRASE.to_string());
    seed_established_pair(tmp_a.path(), tmp_b.path(), &pw_seed, ALICE_ONION, BOB_ONION)
        .expect("seed established pair");

    let net = LoopbackNet::new();

    // --- First run: only Alice is up, so the send has to queue. ---
    let (ready_a, shutdown_a, task_a) = spawn(tmp_a.path(), &net, ALICE_ONION).await;

    let mut info_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let alice_pubkey = match info_a.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };
    let mut contacts_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let bob_pubkey = match contacts_a.execute(Command::ListContacts).await.unwrap() {
        CommandResult::Contacts(v) => {
            v.first()
                .expect("Alice was seeded with Bob as a contact")
                .pubkey
        }
        other => panic!("expected Contacts, got {other:?}"),
    };

    let mut send_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    match tokio::time::timeout(
        Duration::from_secs(30),
        send_a.execute(Command::SendMessage {
            contact: bob_pubkey,
            kind: Kind::Text { body: BODY.into() },
        }),
    )
    .await
    .expect("SendMessage returns within 30 s even with the peer unreachable")
    .unwrap()
    {
        CommandResult::MessageSent { .. } => {}
        other => panic!("expected MessageSent, got {other:?}"),
    }
    let queued = recent(&ready_a.ipc_socket, bob_pubkey)
        .await
        .into_iter()
        .find(|r| matches!(&r.kind, Kind::Text { body } if body == BODY))
        .expect("the send must be persisted in Alice's history");
    assert_eq!(
        queued.delivered_at, None,
        "precondition broken: the message was ACKed while Bob was not running"
    );

    // --- Alice stops. Her actor, and its retry tick, go with her. ---
    let _ = shutdown_a.send(());
    tokio::time::timeout(Duration::from_secs(30), task_a)
        .await
        .expect("Alice shuts down within 30 s")
        .unwrap()
        .expect("Alice's first run exits cleanly");

    // --- Bob comes up, then Alice reboots. Nothing else happens. ---
    let (ready_b, shutdown_b, task_b) = spawn(tmp_b.path(), &net, BOB_ONION).await;
    let (ready_a, shutdown_a, task_a) = spawn(tmp_a.path(), &net, ALICE_ONION).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let got = recent(&ready_b.ipc_socket, alice_pubkey)
            .await
            .into_iter()
            .any(|r| matches!(&r.kind, Kind::Text { body } if body == BODY));
        if got {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "#234: the message queued before Alice's restart never reached Bob. \
             Bob was up before Alice rebooted, and 120 s clears the 15 s and \
             60 s dial rungs, so Alice's restarted daemon never retried the \
             row: no actor was spawned for it at boot."
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Alice sees the ACK, so the outbox row was drained, not just written.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let row = recent(&ready_a.ipc_socket, bob_pubkey)
            .await
            .into_iter()
            .find(|r| matches!(&r.kind, Kind::Text { body } if body == BODY))
            .expect("Alice's own sent message stays in her history");
        if row.delivered_at.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Bob received the message but Alice's row is still un-ACKed 60 s later"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
