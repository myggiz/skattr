// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! In-process `Transport` for tests: a shared registry maps fake onion
//! addresses to an inbound-stream sender; `dial` makes a `duplex` pair and
//! hands one half to the target. No Tor.

#![cfg(feature = "test-harness")]

use crate::error::{CoreError, Result};
use crate::identity::Seed;
use crate::transport::{InboundStreams, Transport, TransportErrorKind, ONION_PORT};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;

const DUPLEX_BUF: usize = 64 * 1024;
const INBOUND_CAP: usize = 64;

/// Shared in-process "network": fake onion -> inbound sender. Clone to share
/// between two `LoopbackTransport`s so they can dial each other.
#[derive(Clone, Default)]
pub struct LoopbackNet(Arc<Mutex<HashMap<String, mpsc::Sender<DuplexStream>>>>);

impl LoopbackNet {
    /// Create an empty shared net with no published onions.
    pub fn new() -> Self {
        Self::default()
    }
}

/// In-process [`Transport`] bound to a single fake onion on a [`LoopbackNet`].
pub struct LoopbackTransport {
    net: LoopbackNet,
    my_onion: String,
}

impl LoopbackTransport {
    /// Create a transport whose published onion is `my_onion` on the shared net.
    pub fn new(net: LoopbackNet, my_onion: impl Into<String>) -> Self {
        Self {
            net,
            my_onion: my_onion.into(),
        }
    }
}

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    type Stream = DuplexStream;

    async fn publish(
        &self,
        _hs_key_path: &Path,
        _seed: &Seed,
        _nickname: &str,
    ) -> Result<(String, InboundStreams<DuplexStream>)> {
        let (tx, rx) = mpsc::channel::<DuplexStream>(INBOUND_CAP);
        self.net
            .0
            .lock()
            .map_err(|_| CoreError::Transport(TransportErrorKind::Other("loopback lock".into())))?
            .insert(self.my_onion.clone(), tx);
        Ok((self.my_onion.clone(), InboundStreams(rx)))
    }

    async fn dial(&self, onion: &str, _port: u16) -> Result<DuplexStream> {
        let sender = {
            let map = self.net.0.lock().map_err(|_| {
                CoreError::Transport(TransportErrorKind::Other("loopback lock".into()))
            })?;
            map.get(onion).cloned()
        };
        let sender = sender.ok_or_else(|| {
            CoreError::Transport(TransportErrorKind::Other(
                "loopback: onion not published".into(),
            ))
        })?;
        let (mine, theirs) = tokio::io::duplex(DUPLEX_BUF);
        sender.send(theirs).await.map_err(|_| {
            CoreError::Transport(TransportErrorKind::Other("loopback: peer gone".into()))
        })?;
        Ok(mine)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(flavor = "multi_thread")]
    async fn loopback_publish_then_dial_round_trips_bytes() {
        let net = LoopbackNet::new();
        let bob = LoopbackTransport::new(net.clone(), "bob.onion");
        let (_addr, mut inbound) = bob
            .publish(Path::new("/unused"), &Seed::generate().unwrap(), "bob")
            .await
            .unwrap();

        let alice = LoopbackTransport::new(net, "alice.onion");
        let mut a = alice.dial("bob.onion", ONION_PORT).await.unwrap();

        let mut b = inbound.recv().await.expect("bob accepts the inbound stream");
        a.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
