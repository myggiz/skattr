// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Production `Transport` over Arti: publish via the existing `OnionListener`,
//! dial via the Arti client. Wraps an already-bootstrapped `TorRuntime`.

use crate::error::{CoreError, Result};
use crate::identity::Seed;
use crate::transport::listener::OnionListener;
use crate::transport::tor::TorRuntime;
use crate::transport::{InboundStreams, Transport, TransportErrorKind};
use std::path::Path;
use tokio::sync::Mutex;

const INBOUND_CAP: usize = 64;

// used by Daemon::run in Task 7
#[allow(dead_code)]
pub(crate) struct ArtiTransport {
    /// Guards the one-time publish (needs `&mut TorRuntime`) and shutdown
    /// (consumes `TorRuntime`, hence the `Option` so it can be `take`n).
    runtime: Mutex<Option<TorRuntime>>,
    /// Cheap Clone handle for concurrent dials (no lock on the hot path).
    client: arti_client::TorClient<tor_rtcompat::tokio::TokioRustlsRuntime>,
}

// used by Daemon::run in Task 7
#[allow(dead_code)]
impl ArtiTransport {
    pub(crate) fn new(runtime: TorRuntime) -> Self {
        let client = runtime.client().clone();
        Self {
            runtime: Mutex::new(Some(runtime)),
            client,
        }
    }
}

#[async_trait::async_trait]
impl Transport for ArtiTransport {
    type Stream = arti_client::DataStream;

    async fn publish(
        &self,
        hs_key_path: &Path,
        seed: &Seed,
        nickname: &str,
    ) -> Result<(String, InboundStreams<Self::Stream>)> {
        let mut guard = self.runtime.lock().await;
        let rt = guard.as_mut().ok_or_else(|| {
            CoreError::Transport(TransportErrorKind::Other(
                "publish: transport already shut down".into(),
            ))
        })?;
        let onion = rt.publish_onion(hs_key_path, seed, nickname).await?;
        let rend = rt.rend_requests_take().ok_or_else(|| {
            CoreError::Transport(TransportErrorKind::Other(
                "publish: rend_requests already taken".into(),
            ))
        })?;
        let listener = OnionListener::spawn(rend, INBOUND_CAP);
        Ok((onion, InboundStreams(listener.into_accepted())))
    }

    async fn dial(&self, onion: &str, port: u16) -> Result<Self::Stream> {
        let target = format!("{onion}:{port}");
        self.client.connect(target.as_str()).await.map_err(|e| {
            CoreError::Transport(TransportErrorKind::Other(format!("connect {target}: {e}")))
        })
    }

    async fn shutdown(&self) -> Result<()> {
        // `TorRuntime::shutdown` consumes `self`, so `take` the runtime out of
        // the `Option` and consume it. A second shutdown is a no-op.
        let runtime = self.runtime.lock().await.take();
        match runtime {
            Some(rt) => rt.shutdown().await,
            None => Ok(()),
        }
    }
}
