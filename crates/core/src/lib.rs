// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

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

pub(crate) mod attachment;
pub(crate) mod delivery;

/// Public re-export of the attachment manifest type for Phase 3.D UI shell.
/// Allows `skattr-ui` to decode `Kind::File` manifests without widening the
/// `attachment` module's overall `pub(crate)` visibility.
pub use attachment::manifest::AttachmentManifest;
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
    // #93 guardrail: the `Transport` trait + `InboundStreams` + `ONION_PORT` so a
    // fault-injecting wrapper transport (drop-first-Ack) can be built in the test
    // crate and drive the real assembly over a wrapped loopback transport.
    pub use crate::transport::{InboundStreams, Transport, ONION_PORT};
    // Phase 1.C additions:
    pub use crate::mls::{Group, GroupId, GroupState, KeyPackage, MlsProvider};
    pub use crate::storage::{KeyPackageRepo, MlsGroupRepo};
    // #93 guardrail: observe the durable first-contact-pending signal directly.
    pub use crate::storage::PendingWelcomeRepo;
    // Phase 1.D additions:
    pub use crate::contact::{Contact, ContactCard, ContactCardBody};
    pub use crate::invite::{InviteLink, InviteLinkBody, InvitePsk};
    // Phase 1.E additions:
    pub use crate::delivery::kill_stream::{KillSwitch, KillableStream};
    pub use crate::delivery::{
        receive, DeliveryHub, DeliveryJob, InboundDispatch, Outbox, OutboxEntry, PeerConnection,
        PeerCtrl, ReceiveOutcome, REPLAY_WINDOW_MS,
    };

    // Phase 2.A addition: build a `DeliveryHub` that owns BOTH an inbound-MLS
    // `dispatch` and an on-demand outbound dialer, so a cross-crate harness can
    // drive the dial-first `add_contact` path (ADR 0009, T1-1) without widening
    // the crate-private `OutboundDial` trait or `new_with_inbound_and_dialer`
    // constructor. The injected dialer runs a real, self-consistent Noise_XK
    // handshake over an in-process `tokio::io::duplex` and hands back the
    // initiator connection plus its `h_transport`; the responder half is dropped
    // (the subsequent best-effort Welcome send over the conn fails quietly — the
    // peer is never actually reached, which suits the local-state assertions in
    // `cli_two_daemons`). It targets a throwaway responder static (NOT the real
    // peer key, which the harness does not hold), so this does not exercise the
    // binding round-trip — only that the mandatory committer dial resolves.
    /// Build a `DeliveryHub` wired with both an inbound-MLS `dispatch` and a
    /// stub on-demand dialer, for cross-crate harnesses exercising the
    /// dial-first `add_contact` path. See the surrounding comment for the
    /// stub's exact behaviour and limits.
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn delivery_hub_with_stub_dialer(
        pool: std::sync::Arc<crate::storage::Pool>,
        dispatch: std::sync::Arc<dyn crate::delivery::InboundDispatch>,
    ) -> std::sync::Arc<crate::delivery::DeliveryHub<tokio::io::DuplexStream>> {
        use crate::delivery::dial::OutboundDial;
        use crate::identity::{IdentityKey, PublicKey as IdPublicKey, Seed};
        use crate::transport::{handshake_initiator, handshake_responder, AuthenticatedConnection};

        struct StubDialer;

        #[async_trait::async_trait]
        impl OutboundDial<tokio::io::DuplexStream> for StubDialer {
            async fn dial_at(
                &self,
                peer: IdPublicKey,
                _onion: &str,
            ) -> crate::error::Result<(
                AuthenticatedConnection<tokio::io::DuplexStream>,
                zeroize::Zeroizing<[u8; 32]>,
            )> {
                self.dial(peer).await
            }

            async fn dial(
                &self,
                _peer: IdPublicKey,
            ) -> crate::error::Result<(
                AuthenticatedConnection<tokio::io::DuplexStream>,
                zeroize::Zeroizing<[u8; 32]>,
            )> {
                // The initiator must target the responder's ACTUAL static
                // (rs), so derive the target from the throwaway responder
                // identity. Setup errors surface via `?` (this lib module
                // disallows `unwrap`, unlike the in-crate `#[cfg(test)]`
                // dispatch harness this mirrors).
                let init_id = IdentityKey::from_seed(&Seed::generate()?)?;
                let resp_id = IdentityKey::from_seed(&Seed::generate()?)?;
                let peer_x = resp_id.noise_static_public();
                let (a, b) = tokio::io::duplex(64 * 1024);
                let (init_res, _resp_res) = tokio::join!(
                    handshake_initiator(a, &init_id, &peer_x, None),
                    handshake_responder(b, &resp_id, None),
                );
                let (conn, outcome) = init_res?;
                Ok((conn, outcome.h_transport))
            }
        }

        let dialer: std::sync::Arc<dyn OutboundDial<tokio::io::DuplexStream>> =
            std::sync::Arc::new(StubDialer);
        std::sync::Arc::new(crate::delivery::DeliveryHub::new_with_inbound_and_dialer(
            pool, dispatch, dialer,
        ))
    }

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

    // Phase 1B Task 8: loopback twin of `run_with_sink` that drives the same
    // `run_with_transport` assembly over `LoopbackTransport` (no Tor) for the
    // two-daemon guardrail.
    pub use crate::daemon::state::run_loopback;

    // Phase 2.C Task 7: loopback twin that additionally wires a REAL in-process
    // mailbox factory + fast poll cadence, for the end-to-end offline-fallback
    // guardrail (peer offline → direct timeout → mailbox deposit → poll →
    // receive). Pair an `Arc<dyn TestMailboxFactory>` through
    // [`mailbox_connect_factory_from_test`] to obtain the
    // `MailboxConnectFactory` argument.
    pub use crate::daemon::state::run_loopback_with_mailbox;

    // #93 guardrail: loopback twin that drives the assembly over a
    // caller-supplied `Transport` (a fault-injecting wrapper around
    // `LoopbackTransport`), for the first-contact-recovers-after-dropped-Ack
    // guardrail. Otherwise byte-identical to `run_loopback`.
    pub use crate::daemon::state::run_loopback_with_transport;

    /// Adapt a cross-crate [`TestMailboxFactory`] into the crate-private
    /// `MailboxConnectFactory` the daemon assembly consumes, so the
    /// offline-fallback guardrail can hand its in-process mailbox factory to
    /// [`run_loopback_with_mailbox`]. Wraps the existing [`TestFactoryBridge`].
    #[must_use]
    #[allow(private_interfaces)]
    pub fn mailbox_connect_factory_from_test(
        factory: std::sync::Arc<dyn TestMailboxFactory>,
    ) -> std::sync::Arc<dyn crate::mailbox::poll::MailboxConnectFactory> {
        std::sync::Arc::new(TestFactoryBridge::new(factory))
    }

    /// #93 guardrail: open `data_dir`'s pool exactly the way the daemon boots
    /// (vault → `derive_storage_seed` → `Pool::open`) and report whether a
    /// durable `pending_welcomes` row still exists for `peer` — i.e. first
    /// contact is still in progress. Read-only; safe to call concurrently with a
    /// running daemon (SQLite WAL allows readers).
    pub fn first_contact_is_pending(
        data_dir: &std::path::Path,
        passphrase: &zeroize::Zeroizing<String>,
        peer: &crate::identity::PublicKey,
    ) -> crate::error::Result<bool> {
        use crate::identity::derive::derive_storage_seed;
        use crate::identity::vault::Vault;
        use crate::storage::{PendingWelcomeRepo, Pool};

        let vault_path = data_dir.join("identity.vault");
        let (_v, id_for_seed) = Vault::open(&vault_path, passphrase.as_str())?;
        let seed = derive_storage_seed(id_for_seed)?;
        let pool = Pool::open(data_dir, &seed)?;
        PendingWelcomeRepo::new(&pool).is_pending(&peer.0)
    }

    /// Phase 1B Task 10: seed two on-disk vaults as *already-established*
    /// mutual contacts so the bidirectional-delivery guardrail can skip the
    /// (deferred-to-1C) invite→add→Welcome handshake and exercise the
    /// established-contact direct-delivery path.
    ///
    /// For each daemon this opens its `Pool` **exactly** the way
    /// `run_with_sink` / `run_loopback` will (vault → `derive_storage_seed`
    /// → `Pool::open`), so the rows written here are visible once the daemon
    /// boots against the same data dir. After writing, every pool + vault
    /// guard is dropped so the daemon can re-open them.
    ///
    /// What it establishes:
    /// 1. A real shared 2-member MLS group (`A.create_solo` →
    ///    `A.add_member(bob_kp)` → `B.join_from_welcome`), persisted on both
    ///    sides via `MlsGroupRepo`.
    /// 2. Mutual `contacts` rows linked to that group id
    ///    (`ContactRepo::upsert` + `set_group_id`) — so each side's accept
    ///    loop resolves the other via `find_by_noise_x25519`.
    /// 3. A signed `ContactCard` for each peer carrying the *loopback onion*,
    ///    persisted via `ContactRepo::put_card` so the dialer resolves the
    ///    peer's onion through `latest_card(peer).body.onion`. This is the
    ///    make-or-break detail: if `latest_card` returned `None`, the dial
    ///    would fail.
    #[allow(clippy::missing_panics_doc)]
    pub fn seed_established_pair(
        data_dir_a: &std::path::Path,
        data_dir_b: &std::path::Path,
        passphrase: &zeroize::Zeroizing<String>,
        onion_a: &str,
        onion_b: &str,
    ) -> crate::error::Result<()> {
        use crate::contact::{Contact, ContactCard};
        use crate::identity::derive::derive_storage_seed;
        use crate::identity::vault::Vault;
        use crate::mls::{Group, KeyPackage, MlsProvider};
        use crate::storage::{ContactRepo, KeyPackageRepo, MlsGroupRepo, Pool};

        // --- Open each vault the same way the daemon does, derive seeds. ---
        // `derive_storage_seed` consumes the identity, so open twice per side:
        // once to derive the storage seed, once to keep an owned identity.
        let vault_path_a = data_dir_a.join("identity.vault");
        let vault_path_b = data_dir_b.join("identity.vault");

        let (_va_seed, id_a_for_seed) = Vault::open(&vault_path_a, passphrase.as_str())?;
        let seed_a = derive_storage_seed(id_a_for_seed)?;
        let (_va, alice_id) = Vault::open(&vault_path_a, passphrase.as_str())?;

        let (_vb_seed, id_b_for_seed) = Vault::open(&vault_path_b, passphrase.as_str())?;
        let seed_b = derive_storage_seed(id_b_for_seed)?;
        let (_vb, bob_id) = Vault::open(&vault_path_b, passphrase.as_str())?;

        let alice_pub = alice_id.public();
        let bob_pub = bob_id.public();

        // --- Open both pools identically to the daemon boot path. ---
        let pool_a = Pool::open(data_dir_a, &seed_a)?;
        let pool_b = Pool::open(data_dir_b, &seed_b)?;

        // --- Build the shared 2-member MLS group with the REAL identities. ---
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &KeyPackageRepo::new(&pool_b))?;

        let mut alice_group = Group::create_solo(&alice_id, None, None, MlsProvider::new())?;
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None)?;
        let gid = alice_group.id().0.clone();
        let bob_group = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)?;

        alice_group.save(&MlsGroupRepo::new(&pool_a))?;
        bob_group.save(&MlsGroupRepo::new(&pool_b))?;

        // --- Mutual contact rows + group_id link. ---
        let now = crate::daemon::clock::now_unix_seconds();
        let contacts_a = ContactRepo::new(&pool_a);
        contacts_a.upsert(&Contact {
            identity: bob_pub,
            display_name: Some("Bob".to_string()),
            added_at: now,
            card: None,
            muted: false,
        })?;
        contacts_a.set_group_id(&bob_pub, &gid)?;

        let contacts_b = ContactRepo::new(&pool_b);
        contacts_b.upsert(&Contact {
            identity: alice_pub,
            display_name: Some("Alice".to_string()),
            added_at: now,
            card: None,
            muted: false,
        })?;
        contacts_b.set_group_id(&alice_pub, &gid)?;

        // --- Signed ContactCards carrying the LOOPBACK onion so the dialer
        // resolves each peer's onion via `latest_card(peer).body.onion`. ---
        // Card for Bob is signed by Bob's identity (owner = bob), onion =
        // onion_b; persisted into Alice's pool so Alice dials "bob.onion".
        let bob_card = ContactCard::sign(&bob_id, onion_b.to_string(), Vec::new(), 1, 86_400, now)?;
        contacts_a.put_card(&bob_card)?;

        let alice_card =
            ContactCard::sign(&alice_id, onion_a.to_string(), Vec::new(), 1, 86_400, now)?;
        contacts_b.put_card(&alice_card)?;

        // --- Drop pools + vault guards so the daemon can re-open them. ---
        drop(pool_a);
        drop(pool_b);
        Ok(())
    }

    /// Phase 2.C Task 7: seed two on-disk vaults as established contacts for
    /// the offline-fallback guardrail. Mirrors [`seed_established_pair`] but
    /// makes Bob DIRECT-UNREACHABLE from Alice while leaving his mailbox
    /// poll path alive:
    ///
    /// - Alice's `ContactCard` for Bob advertises `bob_direct_onion`
    ///   (deliberately NOT registered on the `LoopbackNet`, so every direct
    ///   dial fails) AND `mailbox_onion` in its mailbox list (so the
    ///   per-peer fallback trigger has somewhere to deposit).
    /// - Bob's `ContactCard` for Alice advertises `alice_onion` (registered),
    ///   so Bob→Alice direct delivery would work (unused here, but keeps the
    ///   pair symmetric and the seeding honest).
    /// - Bob gets a `'mine'` mailbox row pointing at `mailbox_onion`, flipped
    ///   `Reachable`, so his `PollScheduler` spawns an actor at boot.
    ///
    /// Returns `(alice_pub, bob_pub)` for the test's send/subscribe wiring.
    #[allow(clippy::missing_panics_doc, clippy::too_many_arguments)]
    pub fn seed_offline_pair(
        data_dir_a: &std::path::Path,
        data_dir_b: &std::path::Path,
        passphrase: &zeroize::Zeroizing<String>,
        alice_onion: &str,
        bob_direct_onion: &str,
        mailbox_onion: &str,
    ) -> crate::error::Result<(crate::identity::PublicKey, crate::identity::PublicKey)> {
        use crate::contact::{Contact, ContactCard};
        use crate::identity::derive::derive_storage_seed;
        use crate::identity::vault::Vault;
        use crate::mls::{Group, KeyPackage, MlsProvider};
        use crate::storage::{
            ContactRepo, KeyPackageRepo, MailboxRepo, MailboxStatus, MlsGroupRepo, Pool,
        };

        let vault_path_a = data_dir_a.join("identity.vault");
        let vault_path_b = data_dir_b.join("identity.vault");

        let (_va_seed, id_a_for_seed) = Vault::open(&vault_path_a, passphrase.as_str())?;
        let seed_a = derive_storage_seed(id_a_for_seed)?;
        let (_va, alice_id) = Vault::open(&vault_path_a, passphrase.as_str())?;

        let (_vb_seed, id_b_for_seed) = Vault::open(&vault_path_b, passphrase.as_str())?;
        let seed_b = derive_storage_seed(id_b_for_seed)?;
        let (_vb, bob_id) = Vault::open(&vault_path_b, passphrase.as_str())?;

        let alice_pub = alice_id.public();
        let bob_pub = bob_id.public();

        let pool_a = Pool::open(data_dir_a, &seed_a)?;
        let pool_b = Pool::open(data_dir_b, &seed_b)?;

        // Shared 2-member MLS group with the REAL identities.
        let bob_provider = MlsProvider::new();
        let bob_kp = KeyPackage::generate(&bob_id, &bob_provider, &KeyPackageRepo::new(&pool_b))?;
        let mut alice_group = Group::create_solo(&alice_id, None, None, MlsProvider::new())?;
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None)?;
        let gid = alice_group.id().0.clone();
        let bob_group = Group::join_from_welcome(&bob_id, &welcome, None, None, bob_provider)?;
        alice_group.save(&MlsGroupRepo::new(&pool_a))?;
        bob_group.save(&MlsGroupRepo::new(&pool_b))?;

        let now = crate::daemon::clock::now_unix_seconds();

        // Mutual contact rows + group link.
        let contacts_a = ContactRepo::new(&pool_a);
        contacts_a.upsert(&Contact {
            identity: bob_pub,
            display_name: Some("Bob".to_string()),
            added_at: now,
            card: None,
            muted: false,
        })?;
        contacts_a.set_group_id(&bob_pub, &gid)?;

        let contacts_b = ContactRepo::new(&pool_b);
        contacts_b.upsert(&Contact {
            identity: alice_pub,
            display_name: Some("Alice".to_string()),
            added_at: now,
            card: None,
            muted: false,
        })?;
        contacts_b.set_group_id(&alice_pub, &gid)?;

        // Bob's card (in Alice's pool): UNREACHABLE direct onion + a mailbox.
        let bob_card = ContactCard::sign(
            &bob_id,
            bob_direct_onion.to_string(),
            vec![mailbox_onion.to_string()],
            1,
            86_400,
            now,
        )?;
        contacts_a.put_card(&bob_card)?;

        // Alice's card (in Bob's pool): reachable onion, no mailbox.
        let alice_card = ContactCard::sign(
            &alice_id,
            alice_onion.to_string(),
            Vec::new(),
            1,
            86_400,
            now,
        )?;
        contacts_b.put_card(&alice_card)?;

        // Bob owns the mailbox: seed a 'mine' row so his PollScheduler polls it.
        let mb_repo = MailboxRepo::new(&pool_b);
        let mb_id = mb_repo.add_mine(mailbox_onion, now)?;
        mb_repo.mark_status(mb_id, MailboxStatus::Reachable)?;

        drop(pool_a);
        drop(pool_b);
        Ok((alice_pub, bob_pub))
    }

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

    /// Test-only constructor for a production `DaemonInbound` (the real
    /// MLS decrypt + persist + emit dispatcher) as an
    /// `Arc<dyn InboundDispatch>`, with the recipient identity wired so
    /// `dispatch_mailbox` can trial-decrypt + persist held deposits. Used
    /// by the RemoveMailbox-drain integration test (Task 22.5).
    #[must_use]
    pub fn daemon_inbound_dispatch(
        pool: std::sync::Arc<crate::storage::Pool>,
        identity: std::sync::Arc<crate::identity::IdentityKey>,
        events_tx: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
    ) -> std::sync::Arc<dyn crate::delivery::InboundDispatch> {
        let inbound = crate::daemon::inbound::DaemonInbound::new(pool, events_tx);
        inbound.set_identity(identity);
        std::sync::Arc::new(inbound)
    }

    /// Test-only setter that injects a shared inbound dispatcher into a
    /// `DaemonHandle`, mirroring the `handle.set_inbound(...)` call made
    /// in `Daemon::run`. Lets the RemoveMailbox-drain integration test
    /// give the handle a real dispatcher (Task 22.5).
    pub fn daemon_handle_set_inbound<S>(
        handle: &mut crate::daemon::handle::DaemonHandle<S>,
        inbound: std::sync::Arc<dyn crate::delivery::InboundDispatch>,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        handle.set_inbound(inbound);
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

    // ── Phase 3.C offline-attachment integration surface (Task 3C-6/7) ──
    //
    // The crate-private attachment pipeline (strip → chunk → stage →
    // enqueue) and its offline sweep / receive functions are exposed here
    // through thin wrappers so the cross-crate guardrail drives the SAME
    // production functions `Command::SendFile` + `run_with_transport`'s
    // sweep loop + the mailbox poll path do, without widening any
    // production visibility.

    // The attachment storage repos + manifest are already `pub`, but their
    // containing modules are `pub(crate)`; re-export the concrete types.
    pub use crate::attachment::manifest::AttachmentManifest;
    pub use crate::storage::attachments::{AttachmentDepositRepo, AttachmentRepo};

    /// Test-only opaque handle around a real `attachment::store::ChunkStore`.
    /// Wraps the `pub(crate)` store so the guardrail can stage + read chunk
    /// blobs without widening the store's visibility.
    pub struct TestChunkStore {
        pub(crate) inner: std::sync::Arc<crate::attachment::store::ChunkStore>,
    }

    impl TestChunkStore {
        /// Open a `ChunkStore` rooted at `data_dir` (mirrors the production
        /// `ChunkStore::new(data_dir)` used by the hub + chunk sweep).
        #[must_use]
        pub fn new(data_dir: &std::path::Path) -> Self {
            Self {
                inner: std::sync::Arc::new(crate::attachment::store::ChunkStore::new(data_dir)),
            }
        }
    }

    /// Run the REAL sender pipeline `Command::SendFile` uses: strip metadata
    /// → chunk into AEAD ciphertext → stage every chunk in `store` → persist
    /// the `'out'` attachment row → enqueue all chunk deposits **due at
    /// `first_due_at_ms`** (pass `0` to make them due immediately, the
    /// deterministic substitute for the 90 s direct→offline stall window).
    ///
    /// Returns the produced `AttachmentManifest` (its CBOR is what the
    /// recipient persists as the pending `'in'` row, exactly as the
    /// `Kind::File` manifest carries it on the wire).
    #[allow(clippy::missing_panics_doc)]
    pub fn attachment_stage_outbound(
        pool: &crate::storage::Pool,
        store: &TestChunkStore,
        recipient: &crate::identity::PublicKey,
        plaintext: &[u8],
        filename: &str,
        mime: &str,
        first_due_at_ms: i64,
    ) -> crate::error::Result<AttachmentManifest> {
        // 1. Strip metadata (identity passthrough for non-images).
        let (stripped, out_mime) = crate::attachment::strip::strip_metadata(plaintext, mime)?;
        // 2. Chunk into per-chunk AEAD ciphertext + manifest.
        let (manifest, chunks) =
            crate::attachment::chunker::chunk_plaintext(&stripped, filename, &out_mime)?;
        // 3. Stage every ciphertext chunk in the sender's ChunkStore.
        for (i, ct) in chunks.iter().enumerate() {
            let index = u32::try_from(i).map_err(|_| {
                crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                    "attachment_stage_outbound: chunk index overflow".into(),
                ))
            })?;
            store.inner.put(&manifest.attachment_id, index, ct)?;
        }
        // 4. Persist the 'out' attachment row.
        let total_chunks = i64::try_from(manifest.chunks.len()).unwrap_or(i64::MAX);
        AttachmentRepo::new(pool).insert(
            &manifest.attachment_id,
            "out",
            &manifest.to_cbor()?,
            total_chunks,
            0,
        )?;
        // 5. Enqueue all chunk deposits (due at `first_due_at_ms`).
        AttachmentDepositRepo::new(pool).enqueue_all(
            &manifest.attachment_id,
            &recipient.0,
            u32::try_from(manifest.chunks.len()).unwrap_or(u32::MAX),
            first_due_at_ms,
        )?;
        Ok(manifest)
    }

    /// Drive the REAL `delivery::chunk_sweep::run_chunk_sweep` once: read due
    /// `attachment_deposits` rows, resolve the recipient's mailboxes, read the
    /// staged ciphertext from `store`, and deposit each chunk into the mailbox
    /// reachable through `hub`'s fallback factory. `now_ms` controls which rows
    /// are due; `batch` caps deposits per call (loop until `all_deposited`).
    ///
    /// The `hub` must have been built with mailbox fallback wired in (e.g. via
    /// [`delivery_hub_with_mailbox`]); its `MailboxFallbackShared` is the exact
    /// bundle `run_with_transport` hands the production sweep task.
    #[allow(clippy::missing_panics_doc)]
    pub async fn attachment_run_chunk_sweep<S>(
        pool: &crate::storage::Pool,
        hub: &crate::delivery::DeliveryHub<S>,
        store: &TestChunkStore,
        now_ms: i64,
        batch: usize,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        // `hub` must be built with mailbox fallback wired in (e.g. via
        // `delivery_hub_with_mailbox`); without it there is nothing to sweep.
        if let Some(shared) = hub.fallback_shared() {
            crate::delivery::chunk_sweep::run_chunk_sweep(
                pool,
                &shared,
                &store.inner,
                now_ms,
                batch,
            )
            .await;
        }
    }

    /// Build a real production `DaemonInbound` wired for the offline
    /// attachment receive path (`chunk_store` + `download_dir` set), returned
    /// as an `Arc<dyn InboundDispatch>` so the guardrail can drive
    /// `poll_dispatch_once` (which calls `dispatch_attachment_chunk` then
    /// `dispatch_mailbox`). This is the SAME dispatcher `run_with_transport`
    /// installs; `set_chunk_store` / `set_download_dir` mirror its wiring.
    #[must_use]
    pub fn daemon_inbound_for_attachments(
        pool: std::sync::Arc<crate::storage::Pool>,
        identity: std::sync::Arc<crate::identity::IdentityKey>,
        events_tx: tokio::sync::broadcast::Sender<crate::daemon::events::Event>,
        store: &TestChunkStore,
        download_dir: std::path::PathBuf,
    ) -> std::sync::Arc<dyn crate::delivery::InboundDispatch> {
        let inbound = crate::daemon::inbound::DaemonInbound::new(pool, events_tx);
        inbound.set_identity(identity);
        inbound.set_chunk_store(store.inner.clone());
        inbound.set_download_dir(download_dir);
        std::sync::Arc::new(inbound)
    }

    /// Drive the REAL `mailbox::poll::poll_dispatch_once` against the mailbox
    /// at `onion`: fetch pending deposits, hand each ciphertext to `inbound`
    /// (`dispatch_attachment_chunk` → `dispatch_mailbox`), and server-side
    /// delete ONLY the deposits that were dispatched. Returns how many were
    /// dispatched + deleted this tick. This is the exact production receive
    /// cycle a `PollScheduler` actor runs per tick.
    pub async fn attachment_poll_dispatch_once(
        factory: &dyn TestMailboxFactory,
        onion: &str,
        signer: &crate::identity::IdentityKey,
        inbound: &dyn crate::delivery::InboundDispatch,
    ) -> crate::error::Result<usize> {
        let mut handle = factory.connect(onion).await?;
        crate::mailbox::poll::poll_dispatch_once(&mut handle.inner, signer, inbound).await
    }
}
