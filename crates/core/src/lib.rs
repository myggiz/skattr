// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! # Skattr core library
//!
//! `skattr-core` is the protocol library that powers the Skattr CLI, the
//! forthcoming Tauri UI, and the integration test crate.
//!
//! ## Module layout
//!
//! - [`identity`]: long-term Ed25519 identity keys, BIP39 seed phrases,
//!   passphrase-encrypted on-disk vaults.
//! - [`transport`]: framed Noise_XK over Tor v3 onion services, via Arti.
//! - [`mls`]: OpenMLS integration, group state machine, keystore bridge.
//! - [`envelope`]: CBOR application payloads carried inside MLS.
//! - [`invite`]: signed invite links + optional QR rendering.
//! - [`contact`]: contacts, signed `ContactCard`s, address rotation.
//! - [`mailbox`]: client of the mailbox server for offline delivery.
//! - [`delivery`]: outbox, retry, dedup, ACK handling.
//! - [`storage`]: SQLite persistence with migrations.
//! - [`daemon`]: top-level process that owns all long-lived handles.
//!
//! ## Public API boundary
//!
//! Only a handful of modules are part of the stable public API. Everything
//! else is `pub(crate)` to keep the surface small and auditable. See
//! `ARCHITECTURE.md` at the workspace root for the full rationale.

// unsafe_code is denied workspace-wide; the one exception is the
// Windows IPC module, which carries its own `#![allow(unsafe_code)]`.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod contact;
pub mod daemon;
pub mod envelope;
pub mod error;
pub mod identity;
pub mod invite;
pub mod prelude;

pub(crate) mod delivery;
pub mod mailbox;
pub(crate) mod mls;
pub(crate) mod storage;
pub(crate) mod transport;

pub use error::{CoreError, Result};

/// Re-exports for integration tests. Gated on the `test-harness`
/// feature so only tests with the feature enabled can reach these
/// items — **not** part of the stable public API.
#[cfg(feature = "test-harness")]
pub mod test_exports {
    pub use crate::transport::{OnionListener, TorConfig, TorRuntime, TorStatus};
    // Phase 0.D additions:
    pub use crate::storage::{
        ContactRepo, InsertParams, MessageRepo, Pool, ReadStateRepo, SeenMessagesRepo,
    };
    // Phase 1.A additions:
    pub use crate::transport::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
    // Phase 1.B additions:
    pub use crate::transport::{
        handshake_initiator, handshake_responder, AuthenticatedConnection, HandshakeOutcome,
        HANDSHAKE_TIMEOUT,
    };
    pub use crate::transport::{LoopbackNet, LoopbackTransport};
    // Phase 1.C additions:
    pub use crate::mls::{Group, GroupId, GroupState, KeyPackage, MlsProvider};
    pub use crate::storage::{KeyPackageRepo, MlsGroupRepo};
    // Phase 1.D additions:
    pub use crate::contact::{Contact, ContactCard, ContactCardBody};
    pub use crate::invite::{InviteLink, InviteLinkBody, InvitePsk};
    // Phase 1.E additions:
    pub use crate::delivery::kill_stream::{KillSwitch, KillableStream};
    pub use crate::delivery::{
        receive, DeliveryHub, DeliveryJob, InboundDispatch, Outbox, OutboxEntry, PeerConnection,
        PeerCtrl, ReceiveOutcome, REPLAY_WINDOW_MS,
    };

    // Phase 1.F additions:
    pub use crate::daemon::handle::DaemonHandle;
    pub use crate::daemon::ipc::{
        client::{IpcClient, IpcClientError},
        codec::CodecError,
        server::{handle_connection, CommandExecutor},
        wire::{EventFilter, IpcError, IpcRequest, IpcResponse, MAX_IPC_BODY},
    };
    pub use crate::daemon::state::Ready;

    // Phase 1B Task 7: generic daemon assembly entrypoint for the
    // two-daemon loopback guardrail (Task 8).
    pub use crate::daemon::state::run_with_transport;

    // Phase 1.G additions:
    pub use crate::daemon::retention::spawn_sweep;

    // Phase 1.H additions:
    pub use crate::daemon::clock::now_unix_seconds;

    // Phase 2.B additions:
    pub use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec, MailboxFrameKind};
    // Mailbox storage + status types — needed by integration tests that
    // pre-populate `'mine'` rows and assert on status transitions.
    pub use crate::storage::{MailboxRepo, MailboxRow, MailboxStatus};
    /// Test-only helper: seed a `direct` outbox row for `(target, message_id)`.
    /// Mirrors `OutboxRepo::insert_direct` but stays inside the `pub(crate)`
    /// boundary so we don't have to widen the OutboxRepo surface.
    pub fn outbox_seed_direct(
        pool: &crate::storage::Pool,
        target: &[u8],
        message_id: &[u8; 16],
        payload: &[u8],
    ) -> crate::error::Result<()> {
        let repo = crate::storage::outbox::OutboxRepo::new(pool);
        let _ = repo.insert_direct(target, message_id, payload, 0)?;
        Ok(())
    }

    /// Test-only helper: read `self_card_state.version` from the pool.
    /// Used by `Command::RotateOnion` integration tests to assert the
    /// version counter advances.
    pub fn self_card_state_version(pool: &crate::storage::Pool) -> crate::error::Result<u64> {
        pool.with(|c| {
            let v: i64 = c
                .query_row(
                    "SELECT version FROM self_card_state WHERE id = 1",
                    rusqlite::params![],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        format!("self_card_state version: {e}"),
                    ))
                })?;
            Ok(u64::try_from(v).unwrap_or(0))
        })
    }

    /// Test-only helper: count outbox rows for a given `target` pubkey.
    /// Used by drain assertions.
    pub fn outbox_count_for_target(
        pool: &crate::storage::Pool,
        target: &[u8],
    ) -> crate::error::Result<i64> {
        pool.with(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM outbox WHERE target = ?1",
                    rusqlite::params![target],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        format!("count outbox: {e}"),
                    ))
                })?;
            Ok(n)
        })
    }

    /// Test-only helper: convert an `IdentityKey` to its X25519 static
    /// public key for use as `peer_static_x25519` in
    /// `handshake_initiator`. Integration tests cannot reach
    /// `IdentityKey::noise_static_public` directly because it is
    /// `pub(crate)`; this wrapper is gated on `feature = "test-harness"`
    /// so the production API stays narrow.
    #[must_use]
    pub fn noise_public_of(id: &crate::identity::IdentityKey) -> [u8; 32] {
        id.noise_static_public()
    }

    // ── Mailbox integration-test surface (Tasks 25–29) ──────────────
    //
    // The crate-private mailbox client / poll / hub plumbing is hidden
    // behind a `MailboxTestKit` facade so the cross-crate integration
    // test harness can drive the same APIs `Daemon::run` does without
    // widening any production visibility.

    /// Object-safe stream alias re-exported for the integration-test
    /// `MailboxConnectFactory` impl. Mirrors the `pub(crate)` trait in
    /// `mailbox::poll`.
    pub trait MailboxStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
    impl<T> MailboxStream for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

    /// Cross-crate factory trait, mirrors `mailbox::poll::MailboxConnectFactory`
    /// but with a public name so integration tests can implement it.
    /// Bridge type [`TestFactoryBridge`] adapts impls of this trait to the
    /// crate-private trait the production code depends on.
    #[async_trait::async_trait]
    pub trait TestMailboxFactory: Send + Sync + 'static {
        /// Open a fresh connection to the mailbox at `onion`.
        async fn connect(&self, onion: &str) -> crate::error::Result<MailboxClientHandle>;
    }

    /// Erased `MailboxClient<Box<dyn MailboxStream>>` returned by the
    /// integration-test factory. Constructed via
    /// [`MailboxClientHandle::from_stream`].
    pub struct MailboxClientHandle {
        pub(crate) inner:
            crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
    }

    impl MailboxClientHandle {
        /// Wrap a duplex stream for a freshly-connected mailbox session.
        #[must_use]
        pub fn from_stream<S>(onion: String, stream: S) -> Self
        where
            S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
        {
            let boxed: Box<dyn crate::mailbox::poll::MailboxStream> = Box::new(stream);
            Self {
                inner: crate::mailbox::client::MailboxClient::from_stream(onion, boxed),
            }
        }

        /// Delegate to `MailboxClient::probe`.
        pub async fn probe(&mut self, identity_hash: [u8; 32]) -> crate::error::Result<()> {
            self.inner.probe(identity_hash).await
        }

        /// Delegate to `MailboxClient::fetch`.
        pub async fn fetch(
            &mut self,
            identity: &crate::identity::IdentityKey,
        ) -> crate::error::Result<crate::mailbox::protocol::FetchResponse> {
            self.inner.fetch(identity).await
        }

        /// Delegate to `MailboxClient::deposit`.
        pub async fn deposit(
            &mut self,
            recipient_hash: [u8; 32],
            ciphertext: Vec<u8>,
            ttl_request: u32,
        ) -> crate::error::Result<crate::mailbox::protocol::DepositOk> {
            self.inner
                .deposit(recipient_hash, ciphertext, ttl_request)
                .await
        }

        /// Delegate to `MailboxClient::delete`.
        pub async fn delete(
            &mut self,
            identity: &crate::identity::IdentityKey,
            deposit_ids: Vec<[u8; 16]>,
        ) -> crate::error::Result<crate::mailbox::protocol::DeleteOk> {
            self.inner.delete(identity, deposit_ids).await
        }
    }

    /// Adapter: wraps a [`TestMailboxFactory`] impl from the integration
    /// test crate and makes it satisfy the crate-private
    /// `MailboxConnectFactory` trait.
    pub struct TestFactoryBridge {
        inner: std::sync::Arc<dyn TestMailboxFactory>,
    }

    impl TestFactoryBridge {
        /// Create a bridge over a public test factory.
        #[must_use]
        pub fn new(inner: std::sync::Arc<dyn TestMailboxFactory>) -> Self {
            Self { inner }
        }
    }

    #[async_trait::async_trait]
    impl crate::mailbox::poll::MailboxConnectFactory for TestFactoryBridge {
        async fn connect(
            &self,
            onion: &str,
        ) -> crate::error::Result<
            crate::mailbox::client::MailboxClient<Box<dyn crate::mailbox::poll::MailboxStream>>,
        > {
            self.inner.connect(onion).await.map(|h| h.inner)
        }
    }

    /// Test-only constructor for a `DeliveryHub` with mailbox-fallback
    /// wired in. Mirrors the `pub(crate) DeliveryHub::new_with_mailbox_fallback`
    /// constructor used by `Daemon::run`.
    #[must_use]
    pub fn delivery_hub_with_mailbox<S>(
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn crate::delivery::InboundDispatch>>,
        events: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
        factory: std::sync::Arc<dyn TestMailboxFactory>,
        identity: std::sync::Arc<crate::identity::IdentityKey>,
    ) -> std::sync::Arc<crate::delivery::DeliveryHub<S>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let bridge: std::sync::Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
            std::sync::Arc::new(TestFactoryBridge::new(factory));
        std::sync::Arc::new(crate::delivery::DeliveryHub::new_with_mailbox_fallback(
            pool, inbound, events, bridge, identity,
        ))
    }

    /// Test-only constructor for a `DaemonHandle` with the mailbox
    /// subsystem wired in (factory + poller-control sender). Mirrors
    /// the `pub(crate) DaemonHandle::new_with_mailbox` constructor used
    /// by `Daemon::run`.
    ///
    /// Returns the handle alongside the matching `(ctrl_tx, ctrl_rx)`
    /// pair so tests can either drive a real `PollScheduler` or just
    /// observe `PollerCtrl` messages.
    #[must_use]
    pub fn daemon_handle_with_mailbox<S>(
        pool: std::sync::Arc<crate::storage::Pool>,
        hub: std::sync::Arc<crate::delivery::DeliveryHub<S>>,
        identity: crate::identity::IdentityKey,
        events_tx: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
        factory: std::sync::Arc<dyn TestMailboxFactory>,
    ) -> (
        std::sync::Arc<crate::daemon::handle::DaemonHandle<S>>,
        tokio::sync::mpsc::Receiver<TestPollerCtrl>,
    )
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        // The handle expects a `Sender<PollerCtrl>` (crate-private). We
        // pair that with a forwarder task that translates intercepted
        // ctrl messages into the public `TestPollerCtrl` mirror so the
        // integration test can observe scheduler notifications.
        let (priv_tx, mut priv_rx) =
            tokio::sync::mpsc::channel::<crate::mailbox::poll::PollerCtrl>(16);
        let (pub_observer_tx, pub_observer_rx) = tokio::sync::mpsc::channel::<TestPollerCtrl>(16);

        tokio::spawn(async move {
            while let Some(msg) = priv_rx.recv().await {
                let mirror = match msg {
                    crate::mailbox::poll::PollerCtrl::AddMailbox(id) => {
                        TestPollerCtrl::AddMailbox(id)
                    }
                    crate::mailbox::poll::PollerCtrl::RemoveMailbox(id) => {
                        TestPollerCtrl::RemoveMailbox(id)
                    }
                    crate::mailbox::poll::PollerCtrl::BumpActive => TestPollerCtrl::BumpActive,
                    crate::mailbox::poll::PollerCtrl::Shutdown => TestPollerCtrl::Shutdown,
                };
                let _ = pub_observer_tx.send(mirror).await;
            }
        });

        let bridge: std::sync::Arc<dyn crate::mailbox::poll::MailboxConnectFactory> =
            std::sync::Arc::new(TestFactoryBridge::new(factory));

        let handle = crate::daemon::handle::DaemonHandle::new_with_mailbox(
            pool, hub, identity, events_tx, bridge, priv_tx,
        );
        (std::sync::Arc::new(handle), pub_observer_rx)
    }

    /// Public mirror of the crate-private `PollerCtrl` enum. The
    /// `daemon_handle_with_mailbox` helper translates between the two so
    /// integration tests can observe scheduler notifications without
    /// reaching into `pub(crate)` types.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum TestPollerCtrl {
        /// A new `'mine'` mailbox row was inserted.
        AddMailbox(i64),
        /// A `'mine'` mailbox row is being removed.
        RemoveMailbox(i64),
        /// Local activity should bump every actor's Active-hold timer.
        BumpActive,
        /// Stop all actors and exit the supervisor.
        Shutdown,
    }

    /// One pending deposit returned by [`mailbox_run_one_poll_tick`].
    #[derive(Debug, Clone)]
    pub struct TestPendingDeposit {
        /// Server-assigned 16-byte deposit id (used by Delete frames).
        pub deposit_id: [u8; 16],
        /// MLS-encrypted ciphertext. The caller decrypts via the matching
        /// `Group::decrypt`.
        pub ciphertext: Vec<u8>,
        /// Server-side received-at unix timestamp.
        pub received_at: i64,
    }

    /// Test-only equivalent of `run_one_poll_tick`. Drives one
    /// Challenge → Fetch → Delete cycle against the mailbox at `onion`
    /// using `factory` to obtain a fresh client connection.
    ///
    /// Returns the list of pending deposits (decryption is the caller's
    /// responsibility — this helper does not inspect ciphertexts). The
    /// deposits are server-side deleted as part of the same tick when
    /// the response is non-empty.
    pub async fn mailbox_run_one_poll_tick(
        factory: &dyn TestMailboxFactory,
        onion: &str,
        signer: &crate::identity::IdentityKey,
    ) -> crate::error::Result<Vec<TestPendingDeposit>> {
        let mut handle = factory.connect(onion).await?;
        let resp = crate::mailbox::poll::run_one_poll_tick(&mut handle.inner, signer).await?;
        Ok(resp
            .deposits
            .into_iter()
            .map(|d| TestPendingDeposit {
                deposit_id: d.deposit_id,
                ciphertext: d.ciphertext,
                received_at: d.received_at,
            })
            .collect())
    }
}
