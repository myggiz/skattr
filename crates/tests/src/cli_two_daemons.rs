// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Two-daemon E2E: invite -> add -> send (blocked while pending), all over a
//! mocked-transport harness.
//!
//! ## What this test covers
//!
//! - Alice creates an invite via IPC (`Command::CreateInvite`).
//! - Bob consumes it via IPC (`Command::AddContact`), establishing a real
//!   2-member MLS group on Bob's side with Alice as the first contact.
//! - Bob sends to Alice (`Command::SendMessage`).
//! - The two hubs are wired together over a `tokio::io::duplex` pair with a
//!   real Noise_XK handshake — the same pattern as
//!   `delivery_kill_mid_message.rs`.
//!
//! ## IPC connection model
//!
//! `handle_connection` is **one-shot per connection**: after each `Execute`
//! request the server writes the result, then writes `Bye` and closes.  The
//! production CLI follows the same model (one OS-level connection per command).
//! Each test therefore creates a fresh `IpcClient` for every `execute()` call
//! via `DaemonBundle::exec()`.
//!
//! ## Why the send is blocked ("not connected yet")
//!
//! `AddContact` creates the 2-member MLS group **only on Bob's side**: Bob is
//! the one consuming Alice's invite and running `Group::create_solo` +
//! `Group::add_member`.  Since #93, that genesis leaves Bob's group
//! `PendingJoin` and persists a `pending_welcomes` row; `SendMessage` is gated
//! on that row (`is_pending`) and returns `InvalidArgument("not connected yet")`
//! until Bob sees Alice's Welcome-Ack.
//!
//! In this harness Alice never joins — her `InboundDispatch` has no group and
//! never returns a Welcome-Ack — so first contact never completes and Bob stays
//! `PendingJoin`.  The send is therefore correctly blocked at the guard (it
//! never reaches delivery).  The true symmetric flow (Alice joins via the real
//! `run_with_transport` accept loop and Acks) is exercised by the
//! `first_contact_*` guardrails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::daemon::commands::MlsGroupStateLabel;
use skattr_core::daemon::error_kind::DaemonErrorKind;
use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::IpcError;
use skattr_core::daemon::{Command, CommandResult, IpcClient, IpcClientError};
use skattr_core::envelope::{Kind, MessageId};
use skattr_core::identity::IdentityKey;
use skattr_core::test_exports::{
    delivery_hub_with_stub_dialer, handle_connection, handshake_initiator, handshake_responder,
    noise_public_of, now_unix_seconds, receive, CommandExecutor, ContactRepo, DaemonHandle, Group,
    GroupId, InboundDispatch, MessageRepo, MlsGroupRepo, Pool, ReceiveOutcome, SeenMessagesRepo,
};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// MlsInboundDispatch — re-used from delivery_kill_mid_message pattern
// ---------------------------------------------------------------------------

/// Test-only `InboundDispatch` that decrypts MLS ciphertexts by reloading
/// the group from storage on every call.  Suitable for a handful of frames.
struct MlsInboundDispatch {
    pool: Arc<Pool>,
    group_id: GroupId,
    expected_peer: skattr_core::identity::PublicKey,
    seen: std::sync::Mutex<std::collections::HashMap<[u8; 32], MessageId>>,
}

impl InboundDispatch for MlsInboundDispatch {
    fn dispatch(
        &self,
        peer: skattr_core::identity::PublicKey,
        ciphertext: &[u8],
    ) -> Option<MessageId> {
        if peer != self.expected_peer {
            return None;
        }
        let hash = sha256(ciphertext);
        if let Some(mid) = self.seen.lock().ok().and_then(|g| g.get(&hash).copied()) {
            return Some(mid);
        }
        let repo = MlsGroupRepo::new(&self.pool);
        let mut g = Group::load(&self.group_id, &repo).ok().flatten()?;
        let envelope = g.decrypt(ciphertext).ok().flatten()?;
        let mls_generation = g.epoch();
        let _ = g.save(&repo);
        let mid = envelope.id;
        if let Ok(mut set) = self.seen.lock() {
            set.insert(hash, mid);
        }
        let now_ms = now_ms();
        let ts_daemon_recv = now_unix_seconds();
        let seen_repo = SeenMessagesRepo::new(&self.pool);
        let msg_repo = MessageRepo::new(&self.pool);
        match receive(
            &self.expected_peer,
            &self.group_id.0,
            envelope,
            now_ms,
            mls_generation,
            ts_daemon_recv,
            &seen_repo,
            &msg_repo,
            true,
        )
        .ok()?
        {
            ReceiveOutcome::New { .. } | ReceiveOutcome::Duplicate => Some(mid),
            ReceiveOutcome::Rejected(_) => None,
        }
    }
}

/// No-op dispatch — for sides that have no decryptable group.
struct NoopInbound;
impl InboundDispatch for NoopInbound {
    fn dispatch(&self, _: skattr_core::identity::PublicKey, _: &[u8]) -> Option<MessageId> {
        None
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// ExecShim: Arc<DaemonHandle<DuplexStream>> → Arc<dyn CommandExecutor>
// ---------------------------------------------------------------------------

type DuplexHandle = DaemonHandle<tokio::io::DuplexStream>;

struct ExecShim(Arc<DuplexHandle>);

#[async_trait::async_trait]
impl CommandExecutor for ExecShim {
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError> {
        self.0.execute(cmd).await
    }
}

// ---------------------------------------------------------------------------
// DaemonBundle — a handle + transport identity + pool
// ---------------------------------------------------------------------------

/// Holds everything needed to operate one test daemon.
struct DaemonBundle {
    /// The IPC-dispatchable handle (contains hub, pool, identity, events).
    handle: Arc<DuplexHandle>,
    /// Separate Arc for the identity used by Noise handshake helpers.
    /// (`IdentityKey` is not `Clone`; we derive a second copy from the same
    /// seed so transport wiring has an independent owned copy.)
    identity: Arc<IdentityKey>,
    /// Direct access to the pool for in-test assertions.
    pool: Arc<Pool>,
}

impl DaemonBundle {
    /// Build a daemon with a `NoopInbound` dispatch.  Adequate for a sender
    /// side or for Alice, who has no group to decrypt.
    fn new_noop() -> Self {
        use skattr_core::identity::Seed;
        let seed = Seed::generate().unwrap();
        let pool = Arc::new(Pool::in_memory());
        let (events_tx, _) = broadcast::channel::<Event>(16);
        let dispatch: Arc<dyn InboundDispatch> = Arc::new(NoopInbound);
        // Phase 2.A: `add_contact` is now dial-first (`connect_and_ingest_at`,
        // ADR 0009 / T1-1), so a hub with no dialer errors "no dialer wired"
        // before any local write. Wire a stub dialer (real Noise_XK handshake
        // over an in-process duplex, throwaway responder) so the mandatory dial
        // resolves; the peer is never actually reached, which is fine — these
        // tests assert only local state (contact persisted, double-use rejected,
        // recent-messages empty, message queued). Production direct→peer
        // delivery is covered by the loopback guardrails.
        let hub = delivery_hub_with_stub_dialer(pool.clone(), dispatch);

        // Handle owns one copy of the identity.
        let id_for_handle = IdentityKey::from_seed(&seed).unwrap();
        let handle = Arc::new(DaemonHandle::new(
            pool.clone(),
            hub,
            id_for_handle,
            events_tx,
        ));

        // Second copy from the same seed for Noise handshake wiring.
        let identity = Arc::new(IdentityKey::from_seed(&seed).unwrap());

        Self {
            handle,
            identity,
            pool,
        }
    }

    /// Send one command.
    ///
    /// Creates a **fresh connection** for each call because `handle_connection`
    /// is one-shot (it closes after the first `Execute` + `Bye`).  This mirrors
    /// how the production CLI works: one `UnixStream` per command.
    async fn exec(&self, cmd: Command) -> std::result::Result<CommandResult, IpcClientError> {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(self.handle.clone()));
        let events_tx = self.handle.events_tx.clone();
        tokio::spawn(async move {
            handle_connection(server_io, exec, events_tx).await;
        });
        IpcClient::from_stream(client_io).execute(cmd).await
    }

    /// Convenience: public key of this daemon's identity.
    fn pubkey(&self) -> skattr_core::identity::PublicKey {
        self.handle.identity.public()
    }
}

// ---------------------------------------------------------------------------
// Transport wiring helper
// ---------------------------------------------------------------------------

/// Perform a paired Noise_XK handshake over a fresh `tokio::io::duplex` pair.
/// Returns `(initiator_conn, responder_conn)`.
async fn paired_handshake(
    initiator: Arc<IdentityKey>,
    responder_id: Arc<IdentityKey>,
) -> (
    skattr_core::test_exports::AuthenticatedConnection<tokio::io::DuplexStream>,
    skattr_core::test_exports::AuthenticatedConnection<tokio::io::DuplexStream>,
) {
    let (a_raw, b_raw) = tokio::io::duplex(64 * 1024);
    let resp_static = noise_public_of(&responder_id);
    let responder_task =
        tokio::spawn(async move { handshake_responder(b_raw, &responder_id, None).await });
    let (init_conn, _) = handshake_initiator(a_raw, &initiator, &resp_static, None)
        .await
        .unwrap();
    let (resp_conn, _) = responder_task.await.unwrap().unwrap();
    (init_conn, resp_conn)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Core flow: Alice mints invite → Bob consumes → Bob sends.
///
/// Both hubs are wired via Noise_XK over `tokio::io::duplex`.  The send
/// result is `Queued` because Alice has no MLS group to ACK with (see
/// module-level doc).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_flow_invite_add_send_queued() {
    let alice = DaemonBundle::new_noop();
    let bob = DaemonBundle::new_noop();

    // Alice needs an onion address to mint an invite.
    alice.handle.set_onion("alice-mock.onion".to_string());

    // --- Wire hubs together via Noise_XK ---
    let (alice_conn, bob_conn) =
        paired_handshake(alice.identity.clone(), bob.identity.clone()).await;
    alice.handle.hub.ingest(bob.pubkey(), alice_conn).await;
    bob.handle.hub.ingest(alice.pubkey(), bob_conn).await;

    // --- Alice creates invite ---
    let invite_url = match alice
        .exec(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
        })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };
    assert!(
        invite_url.starts_with("skattr://invite/v1#"),
        "invite URL must use canonical scheme: {invite_url}"
    );

    // --- Bob consumes invite ---
    let alice_summary = match bob.exec(Command::AddContact { invite_url }).await.unwrap() {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };
    assert_eq!(
        alice_summary.pubkey,
        alice.pubkey(),
        "summary pubkey must be Alice's identity"
    );

    // Verify Bob has a non-empty group_id set for Alice.
    let gid_bytes = ContactRepo::new(&bob.pool)
        .get_group_id(&alice_summary.pubkey)
        .unwrap()
        .unwrap_or_default();
    assert!(
        !gid_bytes.is_empty(),
        "Bob must have a non-empty group_id for Alice"
    );

    // #101: right after AddContact — before the Welcome→Ack round-trip clears the
    // pending_welcomes send-guard — Bob's group with Alice is PendingJoin, not
    // Active. ListContacts derives group_state from MlsGroupRepo (row present →
    // Active) but overrides it to PendingJoin while the pending_welcomes row
    // exists (#93 send-guard), so a not-yet-confirmed first contact is never
    // reported as successfully connected. The send below confirms the same state:
    // it is rejected with "not connected yet".
    {
        let bob_contacts = bob.exec(Command::ListContacts).await.unwrap();
        let alice_entry = match bob_contacts {
            CommandResult::Contacts(ref v) => {
                v.iter().find(|s| s.pubkey == alice.pubkey()).cloned()
            }
            other => panic!("expected Contacts, got {other:?}"),
        };
        let alice_entry =
            alice_entry.expect("Bob must have Alice in contact list after AddContact");
        assert_eq!(
            alice_entry.group_state,
            Some(MlsGroupStateLabel::PendingJoin),
            "#101: Bob's group with Alice must be PendingJoin in ListContacts after \
             AddContact, until the Welcome→Ack clears the send-guard"
        );
    }

    // --- Bob tries to send to Alice while PendingJoin ---
    //
    // #93 Task 3: SendMessage while PendingJoin must be rejected with a clear
    // InvalidArgument error ("not connected yet"), not silently enqueued.
    // The production send-guard checks pending_welcomes before encrypting.
    let send_err = tokio::time::timeout(
        Duration::from_secs(4),
        bob.exec(Command::SendMessage {
            contact: alice_summary.pubkey,
            kind: Kind::Text {
                body: "hello alice".into(),
            },
        }),
    )
    .await
    .expect("send_message must complete within 4 s")
    .unwrap_err();

    match send_err {
        IpcClientError::Server(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            ref message,
        })) => {
            assert!(
                message.contains("not connected yet"),
                "#93: error message must say 'not connected yet', got: {message}"
            );
        }
        other => panic!("#93: expected InvalidArgument(not connected yet), got {other:?}"),
    }
}

/// Alice's contact list is empty after minting an invite (she never ran
/// `AddContact` herself, so no contact row is created for Bob).
#[tokio::test]
async fn alice_has_no_contacts_after_minting_invite() {
    let alice = DaemonBundle::new_noop();
    alice.handle.set_onion("alice-mock.onion".to_string());

    // Mint an invite — must not create a contact row.
    let _ = alice
        .exec(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
        })
        .await
        .unwrap();

    match alice.exec(Command::ListContacts).await.unwrap() {
        CommandResult::Contacts(rows) => assert!(
            rows.is_empty(),
            "Alice must have no contacts after minting an invite only"
        ),
        other => panic!("expected Contacts([]), got {other:?}"),
    }
}

/// Consuming the same invite URL twice must be rejected on the second attempt.
#[tokio::test]
async fn add_contact_rejects_double_use() {
    let alice = DaemonBundle::new_noop();
    alice.handle.set_onion("alice-mock.onion".to_string());

    let invite_url = match alice
        .exec(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
        })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    let bob = DaemonBundle::new_noop();

    // First use succeeds.
    assert!(
        matches!(
            bob.exec(Command::AddContact {
                invite_url: invite_url.clone()
            })
            .await
            .unwrap(),
            CommandResult::ContactAdded(_)
        ),
        "first AddContact must succeed"
    );

    // Second use is rejected.
    match bob.exec(Command::AddContact { invite_url }).await {
        Err(IpcClientError::Server(IpcError::Daemon(DaemonErrorKind::InviteConsumed))) => {}
        other => panic!("expected InviteConsumed, got {other:?}"),
    }
}

/// Bob's `RecentMessages` returns an empty list immediately after `AddContact`
/// (no messages sent yet).
#[tokio::test]
async fn bob_recent_messages_empty_before_send() {
    let alice = DaemonBundle::new_noop();
    alice.handle.set_onion("alice-mock.onion".to_string());

    let invite_url = match alice
        .exec(Command::CreateInvite {
            nickname: None,
            ttl_secs: Some(3600),
        })
        .await
        .unwrap()
    {
        CommandResult::InviteCreated { url, .. } => url,
        other => panic!("expected InviteCreated, got {other:?}"),
    };

    let bob = DaemonBundle::new_noop();
    let alice_summary = match bob.exec(Command::AddContact { invite_url }).await.unwrap() {
        CommandResult::ContactAdded(s) => s,
        other => panic!("expected ContactAdded, got {other:?}"),
    };

    match bob
        .exec(Command::RecentMessages {
            contact: Some(alice_summary.pubkey),
            limit: 10,
            before_id: None,
            paged: false,
        })
        .await
        .unwrap()
    {
        CommandResult::Messages(rows) => {
            assert!(
                rows.is_empty(),
                "no messages sent yet — expected empty list"
            );
        }
        other => panic!("expected Messages, got {other:?}"),
    }
}
