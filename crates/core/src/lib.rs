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

#![forbid(unsafe_code)]
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
pub(crate) mod mailbox;
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
    pub use crate::storage::{ContactRepo, MessageRepo, Pool};
    // Phase 1.A additions:
    pub use crate::transport::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
    // Phase 1.B additions:
    pub use crate::transport::{
        handshake_initiator, handshake_responder, AuthenticatedConnection, HandshakeOutcome,
        HANDSHAKE_TIMEOUT,
    };
    // Phase 1.C additions:
    pub use crate::mls::{Group, GroupId, GroupState, KeyPackage};
    pub use crate::storage::{KeyPackageRepo, MlsGroupRepo};

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

    /// Test-only helper: construct an `MlsProvider` for integration
    /// tests that can't reach the `pub(crate)` module directly.
    #[must_use]
    pub fn new_mls_provider() -> crate::mls::provider::MlsProvider {
        crate::mls::provider::MlsProvider::new()
    }

    /// Test-only helper: open an in-memory `Pool` with migrations applied.
    /// Skips all file I/O and age encryption — suitable for unit and
    /// integration tests only.
    #[must_use]
    pub fn new_in_memory_pool() -> crate::storage::Pool {
        crate::storage::Pool::in_memory_for_test()
    }

    /// Test-only helper: construct a `KeyPackageRepo` bound to `pool`.
    #[must_use]
    pub fn new_kp_repo(pool: &crate::storage::Pool) -> crate::storage::KeyPackageRepo<'_> {
        crate::storage::KeyPackageRepo::new_for_test(pool)
    }

    /// Test-only helper: construct an `MlsGroupRepo` bound to `pool`.
    #[must_use]
    pub fn new_group_repo(pool: &crate::storage::Pool) -> crate::storage::MlsGroupRepo<'_> {
        crate::storage::MlsGroupRepo::new_for_test(pool)
    }
}
