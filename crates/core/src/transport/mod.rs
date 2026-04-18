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
pub(crate) mod frame;
pub(crate) mod hs_key;
pub(crate) mod listener;
pub(crate) mod noise;
pub mod tor;

pub(crate) use connection::AuthenticatedConnection;
pub(crate) use frame::{Frame, FrameCodec, FrameType};
pub(crate) use listener::OnionListener;
pub(crate) use tor::{TorRuntime, TorStatus};
