// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! A handshake-complete bidirectional connection to an authenticated peer.
//!
//! Wraps a `Framed<S, FrameCodec>` and a post-handshake
//! `snow::TransportState`, where `S: AsyncRead + AsyncWrite + Unpin`.
//! Produced by [`super::noise::handshake_initiator`] /
//! [`super::noise::handshake_responder`] — do not construct directly.
//!
//! ## Frame-in-frame semantics
//!
//! [`Self::send`] takes a [`Frame`], serialises it under the
//! `FrameCodec`, encrypts the resulting bytes with the Noise transport
//! cipher, and emits a single [`Frame::MlsApp`] wrapper on the wire.
//! [`Self::recv`] inverts: one `MlsApp` in, one decrypted `Frame` out.
//! The outer `MlsApp` wrapper is what any observer sees; the inner
//! `Frame` is what the application handles. Control frames (`Ping`,
//! `Bye`, `Error`, …) go through the same encrypted envelope — there
//! is no plaintext path post-handshake.

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;
use zeroize::Zeroizing;

use crate::error::Result;
use crate::transport::frame::{Frame, FrameCodec};

/// A Noise-protected, framed stream to a peer whose X25519 static
/// public key has been verified via the Noise_XK handshake.
pub struct AuthenticatedConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    peer_x25519: [u8; 32],
    h_transport: Zeroizing<[u8; 32]>,
    framed: Framed<S, FrameCodec>,
    transport: snow::TransportState,
}

impl<S> AuthenticatedConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Construct from a post-handshake stream. `pub(crate)` because
    /// the only legitimate construction path is through the noise
    /// handshake functions.
    pub(crate) fn new(
        peer_x25519: [u8; 32],
        h_transport: Zeroizing<[u8; 32]>,
        framed: Framed<S, FrameCodec>,
        transport: snow::TransportState,
    ) -> Self {
        Self {
            peer_x25519,
            h_transport,
            framed,
            transport,
        }
    }

    /// Peer's verified X25519 static public key.
    #[must_use]
    pub fn peer_x25519(&self) -> &[u8; 32] {
        &self.peer_x25519
    }

    /// Transport↔MLS binding hash
    /// (`HKDF(noise_handshake_hash, "skattr-binding-v1")`).
    /// Phase 1.C injects this as an external PSK into the first MLS Commit.
    #[must_use]
    pub fn h_transport(&self) -> &[u8; 32] {
        &self.h_transport
    }

    /// Encrypt `frame` under the Noise transport cipher and send the
    /// resulting ciphertext as a single `Frame::MlsApp` on the wire.
    pub async fn send(&mut self, _frame: Frame) -> Result<()> {
        todo!("encode inner frame, snow::TransportState::write_message, wrap in MlsApp")
    }

    /// Read the next `Frame::MlsApp`, decrypt its payload, and decode
    /// the inner [`Frame`]. Returns `Ok(None)` on clean EOF.
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        todo!("StreamExt::next, unwrap MlsApp, TransportState::read_message, decode inner")
    }

    /// Graceful close: send `Frame::Bye`, flush, drop the stream.
    pub async fn close(self) -> Result<()> {
        todo!("self.send(Frame::Bye), then drop framed")
    }
}
