// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! #93 load-bearing guardrail: **first contact recovers after the responder's
//! first Ack is dropped on the wire**, proven through the REAL
//! `run_with_transport` assembly (the audit rule — not `test_exports`
//! hand-wiring).
//!
//! This closes the #90/#93 test gap for the *lost-Ack* recovery lane. The
//! invitee (Bob) sends the Welcome; the responder (Alice) joins, consumes the
//! invite, and sends `Frame::Ack` back. We drop THAT first Ack exactly once.
//! The invitee therefore never finalizes — its durable `pending_welcomes` row
//! survives, blocking app-frame sends — while the responder is `Active`. The
//! durable `welcome_sweeper` re-sends the identical Welcome, and the
//! responder's idempotent re-Ack (ADR 0012 / Tasks 8–10: `first_contact_acks`
//! lookup → same authenticated `peer_x25519` → re-Ack WITHOUT re-joining)
//! answers it, finalizing first contact.
//!
//! ## Why an Ack-drop, not a Welcome-drop
//!
//! A dropped Welcome is NOT recoverable by the #93 design (the responder's
//! ADR-0007 first-contact carve-out closes an unknown peer's connection after a
//! 10 s single-frame window, and a re-dial discards the fresh `h_transport` the
//! genesis PSK is bound to). That is the disclosed lost-Welcome limitation. The
//! lost-*Ack* lane IS recoverable: the Welcome landed, the responder joined and
//! recorded the `first_contact_acks` row, so the re-sent Welcome is re-Acked on
//! the same live connection. We inject exactly that fault.
//!
//! ## The fault seam
//!
//! We wrap Bob's (the invitee's) `LoopbackTransport` in
//! [`DropFirstAckTransport`]. Its `dial` interposes a relay that copies the
//! invitee→responder (up) direction verbatim (so the Welcome IS delivered and
//! Bob's `add_contact` h_transport capture succeeds) and frame-parses the
//! responder→invitee (down) direction — relaying the Noise responder handshake
//! frame (`0x02`), then dropping the FIRST post-handshake data frame (the
//! `Frame::Ack`) exactly once, then relaying everything after. The connection is
//! kept fully open, so the sweeper's re-sent Welcome + the re-Ack traverse the
//! same live connection. Alice's own transport is never wrapped.
//!
//! ## Observable assertions (not wire bytes)
//!
//! - After the Ack is dropped, the invitee is STILL pending:
//!   `PendingWelcomeRepo::is_pending(&responder_identity)` is `true` (reached via
//!   `test_exports::first_contact_is_pending`), AND Bob→Alice `SendMessage`
//!   returns the "not connected yet" error (no app frame emitted while pending).
//! - After recovery: `is_pending` is `false`, both daemons report the contact
//!   with `group_state == Active`, and a text message round-trips BOTH
//!   directions.
//!
//! ## Bug-catch
//!
//! Reverting Task 10's re-Ack pre-check (so `dispatch_welcome_bootstrap` skips
//! the `first_contact_acks` lookup) makes this test FAIL: the responder rejects
//! the re-sent Welcome with an `unknown kp_ref` (single-use invite already
//! consumed), so the invitee never clears pending and recovery never completes.
//! See the commit message for the observed failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::loopback_harness::{
    config_for, init_vault, subscribe_messages, wait_for_group_active, wait_for_message, PASSPHRASE,
};
use skattr_core::daemon::{Command, CommandResult, IpcClient, Ready};
use skattr_core::envelope::Kind;
use skattr_core::test_exports::{
    first_contact_is_pending, run_loopback, run_loopback_with_transport, InboundStreams,
    LoopbackNet, LoopbackTransport, Transport, MAX_FRAME_SIZE,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------
// Fault-injecting transport: drop the first responder→invitee post-handshake
// data frame (the responder's Welcome Ack) exactly once, keeping the connection
// alive so the durable sweeper's re-send + re-Ack can recover.
// ---------------------------------------------------------------------------

const DUPLEX_BUF: usize = 64 * 1024;

/// Wraps a [`LoopbackTransport`]. `publish` delegates untouched; `dial`
/// interposes a relay that drops the first post-handshake data frame on the
/// responder→invitee direction — the `Frame::Ack` — exactly once across the
/// whole process (guarded by `drops_remaining`), keeping the connection open.
/// Only the invitee's transport is wrapped, so the single drop lands on the
/// first Ack and nothing else.
struct DropFirstAckTransport {
    inner: LoopbackTransport,
    drops_remaining: Arc<AtomicUsize>,
}

impl DropFirstAckTransport {
    fn new(
        net: LoopbackNet,
        my_onion: impl Into<String>,
        drops_remaining: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            inner: LoopbackTransport::new(net, my_onion),
            drops_remaining,
        }
    }
}

#[async_trait::async_trait]
impl Transport for DropFirstAckTransport {
    // A `DuplexStream`-shaped type both for inbound (delegated straight through)
    // and for the relayed dial end, so `publish`'s return type is unchanged and
    // we never need to construct the crate-private `InboundStreams`.
    type Stream = DuplexStream;

    async fn publish(
        &self,
        hs_key_path: &std::path::Path,
        seed: &skattr_core::identity::Seed,
        nickname: &str,
    ) -> skattr_core::error::Result<(String, InboundStreams<DuplexStream>)> {
        // Inbound streams are never wrapped — the fault is on the dial side only.
        self.inner.publish(hs_key_path, seed, nickname).await
    }

    async fn dial(&self, onion: &str, port: u16) -> skattr_core::error::Result<DuplexStream> {
        // The real connection to the peer (what LoopbackTransport hands back).
        let real = self.inner.dial(onion, port).await?;
        // The end we return to the daemon. A relay task shuttles bytes between
        // `local_peer` (paired with the returned `local`) and `real`, dropping
        // the first post-handshake frame in the peer→daemon (down) direction.
        let (local, local_peer) = tokio::io::duplex(DUPLEX_BUF);
        let drops_remaining = self.drops_remaining.clone();
        tokio::spawn(async move {
            relay_dropping_first_ack(local_peer, real, drops_remaining).await;
        });
        Ok(local)
    }
}

/// Relay bytes both ways between the daemon-facing `local` end and the `real`
/// connection to the peer, dropping exactly the first responder Ack on the
/// responder→invitee (down) direction while keeping the connection alive.
///
/// The up direction (invitee→responder, carrying the version preamble + Noise
/// handshake + the Welcome frame) is copied verbatim, so the Welcome reaches the
/// responder and it joins. The down direction (responder→invitee, carrying the
/// framed Noise responder message then the Welcome `Ack`) is frame-parsed: the
/// Noise handshake frame(s) (`0x02`) are relayed, then the FIRST post-handshake
/// data frame (the `Ack`) is dropped once, and everything after is relayed. The
/// connection stays fully open, so the invitee's per-peer actor keeps reusing
/// it; the durable sweeper re-sends the Welcome on that same connection, the
/// responder re-Acks (idempotent), and the second Ack — the drop budget now
/// spent — passes through, finalizing first contact.
async fn relay_dropping_first_ack(
    local: DuplexStream,
    real: DuplexStream,
    drops_remaining: Arc<AtomicUsize>,
) {
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let (real_rd, mut real_wr) = tokio::io::split(real);

    // invitee → responder (up): verbatim, so the Welcome is delivered.
    let up = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut local_rd, &mut real_wr).await;
    });
    // responder → invitee (down): drop the first Ack, keep the conn alive.
    let _ = pump_down_dropping_first_ack(real_rd, &mut local_wr, &drops_remaining).await;
    let _ = up.await;
}

/// Relay the responder→invitee framed stream, dropping exactly the first
/// post-handshake data frame (the Welcome `Ack`) once when `drops_remaining`
/// allows. The responder sends NO raw version preamble (only the initiator
/// does), so this parses `length(u32 BE)|type(u8)|payload` frames from byte 0:
/// it relays the Noise responder handshake frame(s) (`0x02`), drops the first
/// non-handshake frame once, and relays everything else verbatim. Returns on
/// EOF. Never tears the connection down — the reused connection is exactly what
/// lets the sweeper's re-sent Welcome (and the re-Ack) succeed.
async fn pump_down_dropping_first_ack<R, W>(
    mut rd: R,
    wr: &mut W,
    drops_remaining: &Arc<AtomicUsize>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        // length prefix (4 bytes, BE) covering type+payload.
        let mut len_buf = [0u8; 4];
        if !read_exact_or_eof(&mut rd, &mut len_buf).await? {
            return Ok(());
        }
        let length = u32::from_be_bytes(len_buf) as usize;
        // Mirror FrameCodec's bounds so a corrupt stream can't OOM the relay.
        if length == 0 || length > MAX_FRAME_SIZE {
            wr.write_all(&len_buf).await?;
            tokio::io::copy(&mut rd, wr).await?;
            return Ok(());
        }
        let mut body = vec![0u8; length]; // type byte + payload
        if !read_exact_or_eof(&mut rd, &mut body).await? {
            return Ok(());
        }
        let frame_type = body[0];
        // Only the Noise responder handshake message is a handshake frame on
        // this direction (`NoiseResp 0x02`); everything after is a data frame.
        let is_handshake = frame_type == 0x02;

        // Drop exactly the first post-handshake data frame — the Welcome Ack —
        // once across the whole process, if a drop budget remains. Keep the
        // connection open (continue relaying) so the sweeper's re-send + re-Ack
        // land on the same live connection.
        if !is_handshake && try_consume_drop(drops_remaining) {
            tracing::info!(
                target: "skattr::tests::drop_ack",
                frame_len = length,
                "dropping the first Welcome Ack (responder→invitee), connection kept alive"
            );
            continue;
        }

        wr.write_all(&len_buf).await?;
        wr.write_all(&body).await?;
        wr.flush().await?;
    }
}

/// Atomically decrement the drop budget iff it is > 0. Returns `true` if this
/// call consumed a drop (i.e. this frame should be swallowed).
fn try_consume_drop(budget: &AtomicUsize) -> bool {
    let mut cur = budget.load(Ordering::SeqCst);
    loop {
        if cur == 0 {
            return false;
        }
        match budget.compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(actual) => cur = actual,
        }
    }
}

/// Read exactly `buf.len()` bytes, or return `Ok(false)` if EOF is hit before
/// any/all bytes arrive (clean stream close). `Ok(true)` on a full read.
async fn read_exact_or_eof<R: AsyncRead + Unpin>(
    rd: &mut R,
    buf: &mut [u8],
) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = rd.read(&mut buf[filled..]).await?;
        if n == 0 {
            return Ok(false);
        }
        filled += n;
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// The guardrail
// ---------------------------------------------------------------------------

/// Two real `run_with_transport` daemons complete first contact even though the
/// responder's FIRST Welcome Ack is dropped on the wire — the durable
/// `welcome_sweeper` re-sends the Welcome and the responder's idempotent re-Ack
/// (ADR 0012) recovers. Proven through the production assembly (no `test_exports`
/// hub hand-wiring). This is the #93 load-bearing guardrail; it fails against a
/// responder without the re-Ack pre-check (Task 10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_contact_recovers_after_dropped_ack() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    init_vault(tmp_a.path());
    init_vault(tmp_b.path());

    let net = LoopbackNet::new();
    let pw = Zeroizing::new(PASSPHRASE.to_string());

    // Global one-shot drop budget: exactly one post-handshake data frame in the
    // responder→invitee direction — the first Ack — is swallowed.
    let drops_remaining = Arc::new(AtomicUsize::new(1));

    // --- Spawn Alice (responder / joiner): plain loopback transport. ---
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

    // --- Spawn Bob (invitee / committer): fault-injecting transport that drops
    // the first responder→invitee post-handshake data frame (the Ack). ---
    let (ready_b_tx, ready_b_rx) = oneshot::channel();
    let (shutdown_b_tx, shutdown_b_rx) = oneshot::channel::<()>();
    let b_dir = tmp_b.path().to_path_buf();
    let b_cfg = config_for(tmp_b.path());
    let b_transport = Arc::new(DropFirstAckTransport::new(
        net.clone(),
        "bob.onion",
        drops_remaining.clone(),
    ));
    let b_pw = pw.clone();
    let task_b = tokio::spawn(async move {
        run_loopback_with_transport(
            &b_dir,
            &b_pw,
            b_cfg,
            std::path::PathBuf::from("/dev/null"),
            b_transport,
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
    assert_eq!(ready_a.onion, "alice.onion");
    assert_eq!(ready_b.onion, "bob.onion");

    // --- Alice creates an invite (embeds her self-card / onion). ---
    let mut client_a = IpcClient::connect(&ready_a.ipc_socket).await.unwrap();
    let invite_url = match tokio::time::timeout(
        Duration::from_secs(30),
        client_a.execute(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(600),
        }),
    )
    .await
    .expect("CreateInvite completes")
    .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    // --- Bob adds the invite: persists Alice's card + genesis group + a durable
    // pending_welcomes row, and nudges the sweeper to deliver the Welcome. The
    // Welcome reaches Alice (up direction is verbatim) and she Acks — but that
    // first Ack is swallowed by Bob's fault-injected transport. AddContact
    // returns promptly (delivery is async). ---
    let mut client_b = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let alice_summary = match tokio::time::timeout(
        Duration::from_secs(30),
        client_b.execute(Command::AddContact {
            invite_url: invite_url.clone(),
        }),
    )
    .await
    .expect("AddContact completes")
    .unwrap()
    {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    let alice_pubkey = alice_summary.pubkey;

    let mut client_b_info = IpcClient::connect(&ready_b.ipc_socket).await.unwrap();
    let bob_pubkey = match client_b_info.execute(Command::DaemonInfo).await.unwrap() {
        CommandResult::DaemonInfo { local_pubkey, .. } => local_pubkey,
        other => panic!("expected DaemonInfo, got {other:?}"),
    };

    // --- Alice joins on the DELIVERED first Welcome, so she reaches Active
    // quickly — this confirms the Welcome landed (the drop was on her Ack, not
    // the Welcome). It is NOT the recovery signal. ---
    wait_for_group_active(&ready_a.ipc_socket, bob_pubkey, Duration::from_secs(60)).await;

    // --- Confirm the fault actually fired: the first Ack was dropped. Alice's
    // Ack rides back shortly after she joins; poll for the one-shot budget to be
    // spent rather than racing the async delivery. ---
    wait_for_drop_consumed(&drops_remaining, Duration::from_secs(30)).await;

    // --- The invitee is STILL pending after the Ack drop. Prove it two ways:
    //   (1) the durable pending_welcomes row survives — is_pending == true; and
    //   (2) Bob→Alice SendMessage is REJECTED with "not connected yet" (no app
    //       frame emitted while pending — the observable of "0 MlsApp on wire").
    // ---
    assert!(
        first_contact_is_pending(tmp_b.path(), &pw, &alice_pubkey).unwrap(),
        "invitee should still be pending after the Ack was dropped (durable row survives)"
    );
    assert_send_blocked_while_pending(&ready_b.ipc_socket, alice_pubkey, Duration::from_secs(10))
        .await;

    // --- THE recovery assertion: the sweeper re-sends the Welcome on the live
    // connection, Alice re-Acks (idempotent, ADR 0012), the second Ack is
    // relayed, and Bob finalizes — deleting the pending_welcomes row and
    // unblocking sends. The recovery signal is Bob's own `SendMessage`
    // succeeding (`send_until_ok`), driven through the daemon's OWN pool + IPC:
    // while pending, `send_message` returns "not connected yet"; once the
    // re-Ack deletes the row, the same command returns `MessageSent`. This is
    // the authoritative daemon-side observable — unlike a second `Pool::open`
    // reader, which cannot see the running daemon's WAL-resident delete. Bound
    // generously (> the sweeper's ACK_TIMEOUT + backoff) so a genuinely stuck
    // re-send fails loudly instead of hanging. ---
    let mut alice_sub = subscribe_messages(&ready_a.ipc_socket, bob_pubkey).await;
    send_until_ok(
        &ready_b.ipc_socket,
        bob_to_alice(alice_pubkey),
        Duration::from_secs(120),
    )
    .await;
    wait_for_message(
        &mut alice_sub,
        bob_pubkey,
        "hello-alice",
        Duration::from_secs(30),
    )
    .await;

    // --- Reverse direction Alice→Bob (Alice learned Bob's onion from his
    // reverse self-card, sent on Ack). Round-trips → first contact fully live. ---
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
    .expect("Alice send completes")
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

    // --- Graceful shutdown. ---
    let _ = shutdown_a_tx.send(());
    let _ = shutdown_b_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), task_b).await;
}

/// Poll the one-shot drop budget until it reaches 0 (the first Ack was dropped),
/// or panic after `timeout`. The Ack drop is async relative to `AddContact`
/// returning (it happens once the Welcome round-trips and Alice Acks), so this
/// waits for it rather than racing it.
async fn wait_for_drop_consumed(budget: &AtomicUsize, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if budget.load(Ordering::SeqCst) == 0 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("the first Welcome Ack was not dropped within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The `SendMessage` command Bob issues to Alice during the pending window.
fn bob_to_alice(alice: skattr_core::identity::PublicKey) -> Command {
    Command::SendMessage {
        contact: alice,
        kind: Kind::Text {
            body: "hello-alice".into(),
        },
    }
}

/// Poll Bob→`peer` `SendMessage` until it is REJECTED with the "not connected
/// yet" pending error (proving the durable pending signal blocks app frames),
/// or panic after `timeout` if it ever succeeds (would mean the invitee wrongly
/// believes it is paired — the #90 bug).
async fn assert_send_blocked_while_pending(
    ipc_path: &std::path::Path,
    peer: skattr_core::identity::PublicKey,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut saw_block = false;
    loop {
        let mut client = IpcClient::connect(ipc_path).await.expect("connect IPC");
        match client.execute(bob_to_alice(peer)).await {
            Ok(CommandResult::MessageSent { .. }) => {
                panic!(
                    "SendMessage succeeded while first contact should still be pending \
                     (invitee wrongly believes it is paired — the #90 bug)"
                );
            }
            Ok(other) => panic!("expected MessageSent or a pending error, got {other:?}"),
            Err(e) => {
                // The pending guard surfaces as DaemonErrorKind::InvalidArgument
                // { message: "not connected yet — waiting for them to join" }.
                let s = format!("{e:?}");
                if s.contains("not connected yet") {
                    saw_block = true;
                    break;
                }
                // Any other transient error: keep polling until the deadline.
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        saw_block,
        "expected a 'not connected yet' pending rejection within {timeout:?}"
    );
}

/// Retry `cmd` until it returns `MessageSent`, or panic after `timeout`. Used
/// post-recovery, where the re-Ack → pending-row-delete → send-unblock is async
/// relative to the pending row clearing. A FRESH IPC connection is used per
/// attempt (mirroring `assert_send_blocked_while_pending`): the daemon returns
/// the "not connected yet" pending rejection as an error response, and a reused
/// client can be left unusable (broken pipe) after such a response, so
/// reconnecting each attempt is required for a reliable poll.
async fn send_until_ok(ipc_path: &std::path::Path, cmd: Command, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut client = IpcClient::connect(ipc_path).await.expect("connect IPC");
        match client.execute(cmd.clone()).await {
            Ok(CommandResult::MessageSent { .. }) => return,
            Ok(other) => panic!("expected MessageSent, got {other:?}"),
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("SendMessage never succeeded within {timeout:?}: {e:?}");
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
