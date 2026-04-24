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

use crate::error::{CoreError, Result};
use crate::transport::frame::{Frame, FrameCodec};
use crate::transport::TransportErrorKind;

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
    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        use futures::SinkExt as _;
        use tokio_util::codec::Encoder as _;

        // Encode the inner frame into a scratch buffer.
        let mut inner = bytes::BytesMut::new();
        let mut codec = FrameCodec::new();
        codec.encode(frame, &mut inner)?;

        // Noise payload cap (65535) minus ChaChaPoly tag (16) = 65519.
        // FrameCodec enforces 16 MiB so the inner could in principle be
        // larger; for 1.B's scope — Ping/Pong/Bye/small MlsApp — inner
        // stays well under 65 KiB. Larger payloads land in 1.E with a
        // chunked send path.
        const NOISE_MAX_OUTER: usize = 65519;
        if inner.len() > NOISE_MAX_OUTER {
            return Err(CoreError::Transport(TransportErrorKind::Other(format!(
                "send: inner frame too large for single Noise message: {} bytes",
                inner.len()
            ))));
        }

        let mut cipher = vec![0u8; inner.len() + 16];
        let n = self
            .transport
            .write_message(&inner, &mut cipher)
            .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("send: {e}"))))?;
        cipher.truncate(n);

        self.framed
            .send(Frame::MlsApp(cipher))
            .await
            .map_err(|e| CoreError::Transport(TransportErrorKind::Other(format!("send: {e}"))))?;
        Ok(())
    }

    /// Read the next `Frame::MlsApp`, decrypt its payload, and decode
    /// the inner [`Frame`]. Returns `Ok(None)` on clean EOF.
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        use futures::StreamExt as _;
        use tokio_util::codec::Decoder as _;

        let next = match self.framed.next().await {
            None => return Ok(None),
            Some(Ok(f)) => f,
            Some(Err(e)) => return Err(e),
        };

        let cipher = match next {
            Frame::MlsApp(bytes) => bytes,
            other => {
                return Err(CoreError::Transport(TransportErrorKind::Other(format!(
                    "recv: expected MlsApp, got type 0x{:02X}",
                    other.frame_type() as u8
                ))));
            }
        };

        let mut clear = vec![0u8; cipher.len()];
        let n = self
            .transport
            .read_message(&cipher, &mut clear)
            .map_err(|e| {
                CoreError::Transport(TransportErrorKind::Other(format!(
                    "recv: authentication failed: {e}"
                )))
            })?;
        clear.truncate(n);

        // Decode the inner frame using a fresh FrameCodec.
        let mut codec = FrameCodec::new();
        let mut buf = bytes::BytesMut::from(&clear[..]);
        match codec.decode(&mut buf)? {
            Some(inner) => {
                if !buf.is_empty() {
                    return Err(CoreError::Transport(TransportErrorKind::Other(
                        "recv: inner frame left trailing bytes".into(),
                    )));
                }
                Ok(Some(inner))
            }
            None => Err(CoreError::Transport(TransportErrorKind::Other(
                "recv: inner frame was incomplete".into(),
            ))),
        }
    }

    /// Graceful close: send `Frame::Bye`, flush, drop the stream.
    /// Errors on the Bye send are swallowed — close is best-effort and
    /// the caller is about to drop the connection anyway.
    pub async fn close(mut self) -> Result<()> {
        let _ = self.send(Frame::Bye).await;
        use futures::SinkExt as _;
        let _ = self.framed.close().await;
        Ok(())
    }
}
