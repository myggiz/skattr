// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! A handshake-complete bidirectional connection to an authenticated peer.

use tokio::sync::mpsc;

use crate::error::Result;
use crate::identity::PublicKey;
use crate::transport::frame::Frame;

/// A Noise-protected, framed stream to a peer whose [`PublicKey`] has
/// been verified via the handshake.
///
/// Produced by the [`super::listener::OnionListener`] on inbound
/// connections and by the sender's dial path on outbound connections.
/// Owns the underlying Tor stream and the Noise transport cipher state.
pub struct AuthenticatedConnection {
    /// Verified peer identity pubkey.
    pub peer: PublicKey,
    /// Outbound frame channel, drained by a background writer task.
    pub outbound: mpsc::Sender<Frame>,
    /// Inbound frame channel, produced by a background reader task.
    pub inbound: mpsc::Receiver<Frame>,
}

impl AuthenticatedConnection {
    /// Send a frame to the peer.
    pub async fn send(&self, _frame: Frame) -> Result<()> {
        todo!("push into outbound; surface send errors")
    }

    /// Receive the next frame from the peer.
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        todo!("poll inbound channel")
    }

    /// Graceful close: send `BYE` and drain.
    pub async fn close(self) -> Result<()> {
        todo!("send Bye, flush, drop streams")
    }
}
