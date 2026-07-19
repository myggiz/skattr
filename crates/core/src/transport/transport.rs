// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Transport abstraction: publish an onion + receive inbound connections,
//! and dial outbound onions. Implemented by `ArtiTransport` (production) and
//! `LoopbackTransport` (in-process, test-harness). Lets `run_with_transport`
//! drive the real daemon assembly in CI without Tor.

use crate::error::Result;
use crate::identity::Seed;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite};

/// Fixed virtual port the onion service listens on and dialers connect to.
pub const ONION_PORT: u16 = 9735;

/// Receiver of inbound, post-rendezvous, pre-handshake byte streams.
pub struct InboundStreams<S>(pub(crate) tokio::sync::mpsc::Receiver<S>);

impl<S> InboundStreams<S> {
    pub(crate) async fn recv(&mut self) -> Option<S> {
        self.0.recv().await
    }
}

/// Publish an onion + accept inbound streams, and dial outbound onions.
/// Implemented by `ArtiTransport` (production) and `LoopbackTransport`
/// (in-process, test-harness); drives `run_with_transport` without Tor in CI.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Concrete byte-stream type. Note: this associated type makes `Transport`
    /// NOT object-safe by design — it is always used as a generic bound
    /// (`T: Transport`), never as `dyn Transport`.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Publish this daemon's onion service; return its address + the stream of
    /// accepted inbound connections. Called once at startup.
    async fn publish(
        &self,
        hs_key_path: &Path,
        seed: &Seed,
        nickname: &str,
    ) -> Result<(String, InboundStreams<Self::Stream>)>;

    /// Open an outbound connection to `onion:port`.
    async fn dial(&self, onion: &str, port: u16) -> Result<Self::Stream>;

    /// Tear down any transport-owned resources. Default: no-op.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
