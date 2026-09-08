// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #227/#229 guardrail: a message queued while the peer is unreachable must
//! deliver **on its own** once the peer comes back — with no new user action
//! and no inbound dial from the peer.
//!
//! ## The field failure this exists to catch
//!
//! Six messages sat queued for 23 hours and moved only when the *peer* dialled
//! in. The whole suite was green throughout, because no test ever exercised a
//! queued message surviving an outage: every existing guardrail either sends
//! while both sides are already reachable (`daemon_run_direct`) or routes
//! around the outage via a mailbox (`offline_fallback`). Neither one needs this
//! side to redial, which is precisely the code path that was missing.
//!
//! ## Shape of the test
//!
//! Alice boots alone on a shared `LoopbackNet`; Bob's daemon is simply **not
//! started**, so the onion Alice's `ContactCard` for Bob advertises is absent
//! from the net's registry and every dial fails at
//! `LoopbackTransport::dial` ("loopback: onion not published"). That is a
//! genuine unreachable peer with no new harness API — no `partition`/`heal`
//! seam is needed, and none is added.
//!
//! Alice sends one message. It cannot reach the wire, so it lands in the
//! outbox; we assert that by reading her own history projection and requiring
//! `delivered_at == None`.
//!
//! Then Bob's daemon starts. **Nothing else happens**: no second send, no IPC
//! command on Bob, no dial from Bob. The only thing that can move the message
//! is Alice's per-peer retry tick dialling for a queued outbox row (the #227
//! block in `delivery::peer`). Reading history is passive and cannot trigger a
//! send, so polling for the result does not weaken the assertion.
//!
//! Delivery is asserted from **both** ends:
//!   * Bob's history contains the body — it really arrived and decrypted;
//!   * Alice's row carries a `delivered_at` — the ACK came back and the outbox
//!     row was removed on that same path (`Outbox::ack` + `mark_delivered` are
//!     driven together by the Ack frame), so this is the outbox-drained
//!     assertion without opening the daemon's live pool from the test.
//!
//! ## Timing — deterministic, but not fast
//!
//! Alice's queued-row dial is paced by `CHUNK_DIAL_BACKOFF_MS` (15 s, 60 s,
//! …). Bob is unreachable for the first one or two rungs, so delivery lands on
//! a later rung; the test therefore runs for ~2 minutes. That is the price of
//! the ordering being unambiguous — Bob's `Ready` is awaited only *after* the
//! send, so he is provably down when the send happens, rather than racing a
//! concurrent boot against the first dial. A debug-build daemon boot is
//! dominated by the Argon2id vault unlock (~15 s), which is the same order as
//! the first backoff rung, so overlapping the two would buy speed at the cost
//! of flakiness.
//!
//! Both waits are deadline-bounded polls with a wide margin, never sleeps
//! sized to the expected latency, and the failure messages name what did not
//! happen. The receive deadline is sized to clear the ladder's third rung
//! (t = 375 s) rather than to the observed ~114 s, so that no timeout in this
//! file is load-bearing on Bob booting before a particular rung — see the
//! comment on that deadline.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Duration;

use skattr_core::daemon::{Command, CommandResult, IpcClient, Ready};
use skattr_core::envelope::Kind;
use skattr_core::identity::PublicKey;
use skattr_core::test_exports::{run_loopback, seed_established_pair, LoopbackNet};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::loopback_harness::{config_for, init_vault, PASSPHRASE};

const ALICE_ONION: &str = "alice-queued.onion";
/// Bob's onion. Alice's ContactCard advertises it from the start, but nothing
/// publishes it on the `LoopbackNet` until Bob's daemon actually boots — which
/// is what makes Alice's dials fail for real.
const BOB_ONION: &str = "bob-queued.onion";

const BODY: &str = "queued while you were away";

/// Read the peer's view of a conversation. Passive: `RecentMessages` never
/// enqueues or dials, so calling it cannot be what delivers the message.
async fn recent(
    ipc: &Path,
    contact: PublicKey,
) -> Vec<skattr_core::daemon::commands::MessageRecord> {
    let mut client = IpcClient::connect(ipc).await.expect("connect IPC");
    match client
        .execute(Command::RecentMessages {
            contact: Some(contact),
            limit: 50,
            before_id: None,
            paged: false,
        })
        .await
        .expect("RecentMessages")
    {
        CommandResult::Messages(v) => v,
        other => panic!("expected Messages, got {other:?}"),
    }
}

/// Boot one loopback daemon and wait for `Ready`.
async fn spawn(
    data_dir: &Path,
    net: &LoopbackNet,
    onion: &str,
) -> (
    Ready,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<skattr_core::error::Result<()>>,
) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let dir = data_dir.to_path_buf();
    let cfg = config_for(data_dir);
    let net = net.clone();
    let pw = Zeroizing::new(PASSPHRASE.to_string());
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
    let ready = tokio::time::timeout(Duration::from_secs(60), ready_rx)
        .await
        .expect("daemon ready within 60 s")
        .expect("ready_tx still open");
    (ready, shutdown_tx, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_delivers_when_peer_returns_over_loopback() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let pw_seed = Zeroizing::new(PASSPHRASE.to_string());
    seed_established_pair(tmp_a.path(), tmp_b.path(), &pw_seed, ALICE_ONION, BOB_ONION)
        .expect("seed established pair");

    let net = LoopbackNet::new();

    // --- Only Alice boots. Bob's onion is unpublished, so dials fail. ---
    let (ready_a, shutdown_a, task_a) = spawn(tmp_a.path(), &net, ALICE_ONION).await;

    // One command per IPC connection: the server closes the stream after a
    // non-subscribe response, so each command opens its own client.
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

    // --- Send while Bob is unreachable: this must queue, not deliver. ---
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
        "precondition broken: the message was ACKed while Bob was not running — \
         the test is no longer exercising an outage"
    );

    // --- Bob returns. From here on NOTHING acts on the message: no new send,
    // no command issued to Bob, no dial from Bob. Only Alice's retry tick can
    // move it. ---
    let (ready_b, shutdown_b, task_b) = spawn(tmp_b.path(), &net, BOB_ONION).await;

    // Bob receives it.
    //
    // The deadline must clear the THIRD rung of `CHUNK_DIAL_BACKOFF_MS`
    // (`peer.rs`: 15 s, 60 s, 300 s, 900 s), which puts Alice's dial attempts
    // at roughly t = 0, 15 s, 75 s, 375 s after her first failure. Anything
    // shorter than ~330 s is load-bearing on Bob being ready before the t=75 s
    // rung: miss it and the next attempt is 300 s later, so a deadline of e.g.
    // 120 s expires first and blames #227 for what is really a slow boot — a
    // flake with a diagnostic pointing at the wrong code. 330 s removes that
    // coupling; the happy path still returns at ~114 s, so normal runtime is
    // unchanged. Raise this if a rung is ever added or lengthened.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(330);
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
            "the queued message never reached Bob: 330 s after Bob came back \
             online, Alice's outbox row had still not been redialled and sent. \
             That is past the t = 15/75/375 s dial rungs of \
             CHUNK_DIAL_BACKOFF_MS, so this is the #227 field failure — a \
             queued message that only moves when the peer dials in."
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Alice sees the ACK: the outbox row was acked and removed, not merely
    // written to the wire.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let delivered_at = loop {
        let row = recent(&ready_a.ipc_socket, bob_pubkey)
            .await
            .into_iter()
            .find(|r| matches!(&r.kind, Kind::Text { body } if body == BODY))
            .expect("Alice's own sent message stays in her history");
        assert_eq!(
            row.failed_reason, None,
            "the message must not be marked failed once it has been delivered"
        );
        if let Some(t) = row.delivered_at {
            break t;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Bob received the message but Alice's row is still un-ACKed 60 s \
             later: delivered_at is NULL, so the outbox row was never drained"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        delivered_at > 0,
        "delivered_at must carry a real timestamp, got {delivered_at}"
    );

    let _ = shutdown_a.send(());
    let _ = shutdown_b.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}
