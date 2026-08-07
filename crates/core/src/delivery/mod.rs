// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Send/receive plumbing: outbox queue, retry, dedup, ACK handling.

pub(crate) mod backoff;
pub(crate) mod chunk_sweep;
pub(crate) mod chunk_transfer;
pub(crate) mod dial;
mod error_kind;
pub(crate) use error_kind::DeliveryErrorKind;
pub(crate) mod hub;
pub(crate) mod mailbox_sweeper;
pub(crate) mod outbox;
pub(crate) mod peer;
pub(crate) mod receiver;
pub(crate) mod welcome_sweep;

#[cfg(feature = "test-harness")]
pub mod kill_stream;

// Under `test-harness` the items need a `pub` path so `lib.rs::test_exports`
// can re-export them. The `delivery` module itself is `pub(crate)`, so these
// `pub use` items are still invisible outside the crate — the effective
// visibility is capped by the module. This is intentional.
#[cfg(feature = "test-harness")]
pub use hub::DeliveryHub;
#[cfg(feature = "test-harness")]
pub use outbox::{Outbox, OutboxEntry};
#[cfg(feature = "test-harness")]
pub use peer::{DeliveryJob, InboundDispatch, PeerConnection, PeerCtrl};
#[cfg(feature = "test-harness")]
pub use receiver::{receive, ReceiveOutcome, REPLAY_WINDOW_MS};
