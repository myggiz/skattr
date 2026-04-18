// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Accepts onion-service connections, yields authenticated `DataStream`s
//! via an mpsc channel.
//!
//! The Noise handshake (§1.3) is applied on top of each `DataStream`
//! in `transport::connection::AuthenticatedConnection`; this listener is
//! the byte-level accept loop only.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use arti_client::DataStream;
use tokio::sync::mpsc;

use crate::error::Result;

/// Accept loop over a v3 onion service.
///
/// Owns a background task that drains the rend-request stream from
/// `tor_hsservice`, accepts each per-circuit `StreamRequest`, and
/// forwards the resulting `DataStream` via `accepted`. Dropping
/// `OnionListener` aborts the task.
pub struct OnionListener {
    /// Channel that yields raw `DataStream`s from accepted rend requests.
    /// Higher layers wrap each stream with Noise + Frame to get an
    /// `AuthenticatedConnection` (Phase 1 work).
    pub accepted: mpsc::Receiver<DataStream>,
    task: tokio::task::JoinHandle<()>,
}

impl OnionListener {
    /// Spawn an accept loop over `rend_requests`. The channel buffers
    /// up to `capacity` pending streams — backpressure slows Arti's
    /// rendezvous accept rate past that.
    ///
    /// `capacity` must be >= 1 (tokio's mpsc panics on 0).
    pub fn spawn(
        rend_requests: impl futures::Stream<Item = tor_hsservice::RendRequest> + Send + 'static,
        capacity: usize,
    ) -> Self {
        let (tx, accepted) = mpsc::channel(capacity);
        let task = tokio::spawn(async move {
            use futures::StreamExt;
            tokio::pin!(rend_requests);
            while let Some(request) = rend_requests.next().await {
                // `RendRequest::accept` is async in tor-hsservice 0.41 and
                // returns `Result<impl Stream<Item = StreamRequest> + Unpin, ClientError>`.
                let stream_requests = match request.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "HS rend request: accept failed");
                        continue;
                    }
                };
                // Each RendRequest can fan out into multiple per-circuit
                // StreamRequest — drain them.
                // The returned stream is already `Unpin` (no tokio::pin! needed).
                let mut stream_requests = stream_requests;
                while let Some(stream_req) = stream_requests.next().await {
                    // `StreamRequest::accept` takes a `Connected` message
                    // (tor_cell::relaycell::msg::Connected). We send an
                    // empty Connected cell (no address) which is the correct
                    // behaviour for a hidden service: the client already knows
                    // the address it dialled.
                    let connected = tor_cell::relaycell::msg::Connected::new_empty();
                    match stream_req.accept(connected).await {
                        Ok(data_stream) => {
                            if tx.send(data_stream).await.is_err() {
                                // Receiver dropped; exit the task.
                                return;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "HS stream request: accept failed");
                        }
                    }
                }
            }
        });

        Self { accepted, task }
    }

    /// Stop the accept loop. Any `DataStream`s already delivered to
    /// `accepted` remain valid; the underlying onion service stays up
    /// until `TorRuntime::shutdown`.
    pub fn shutdown(self) -> Result<()> {
        self.task.abort();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_type_is_send() {
        // Compile-time assertion that OnionListener is Send (so it can
        // cross .await points in the daemon).
        fn assert_send<T: Send>() {}
        assert_send::<OnionListener>();
    }
}
