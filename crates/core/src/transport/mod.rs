// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Transport: framed Noise_XK over Tor v3 onion services.
//!
//! Layered from bottom up:
//!
//! 1. [`tor`] — Arti runtime: bootstrap, publish onion, dial onion.
//! 2. [`frame`] — `tokio_util::codec` Encoder/Decoder for length-prefixed
//!    typed frames.
//! 3. [`noise`] — Noise_XK handshake and transport cipher via `snow`.
//! 4. [`connection`] — [`AuthenticatedConnection`]: a handshake-complete
//!    bidirectional stream with an authenticated peer identity.
//! 5. [`listener`] — accepts onion stream callbacks and feeds them to
//!    the session manager.

pub(crate) mod connection;
mod error_kind;
pub(crate) mod frame;
pub(crate) mod hs_key;
pub(crate) mod listener;
pub(crate) mod noise;
pub(crate) mod tor;

pub(crate) use error_kind::TransportErrorKind;

#[cfg(not(feature = "test-harness"))]
pub(crate) use connection::AuthenticatedConnection;
#[cfg(feature = "test-harness")]
pub use connection::AuthenticatedConnection;

#[cfg(not(feature = "test-harness"))]
pub(crate) use frame::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};
#[cfg(feature = "test-harness")]
pub use frame::{Frame, FrameCodec, FrameType, MAX_FRAME_SIZE};

#[cfg(not(feature = "test-harness"))]
pub(crate) use noise::{
    handshake_initiator, handshake_responder, HandshakeOutcome, HANDSHAKE_TIMEOUT,
};
#[cfg(feature = "test-harness")]
pub use noise::{handshake_initiator, handshake_responder, HandshakeOutcome, HANDSHAKE_TIMEOUT};

// Under `test-harness` the items need a `pub` path so `lib.rs::test_exports`
// can re-export them. The `transport` module itself is `pub(crate)`, so these
// `pub use` items are still invisible outside the crate — the effective
// visibility is capped by the module. This is intentional.
#[cfg(not(feature = "test-harness"))]
pub(crate) use listener::OnionListener;
#[cfg(not(feature = "test-harness"))]
pub(crate) use tor::{TorRuntime, TorStatus};

#[cfg(feature = "test-harness")]
pub use listener::OnionListener;
#[cfg(feature = "test-harness")]
pub use tor::{TorConfig, TorRuntime, TorStatus};
