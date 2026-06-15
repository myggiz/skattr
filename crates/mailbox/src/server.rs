// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-stream accept loop and the top-level [`MailboxServer`] handle.
//!
//! The server is transport-agnostic: any `AsyncRead + AsyncWrite +
//! Unpin + Send` stream can be passed to [`MailboxServer::accept_loop`].
//! The Tor onion lives in `crates/mailbox/src/arti.rs` (binary-only).

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;
use tracing::{debug, warn};

use crate::auth::Challenges;
use crate::codec::{MailboxFrame, MailboxFrameCodec};
use crate::dispatch::{
    error_frame, handle_challenge, handle_delete, handle_deposit, handle_fetch, DispatchCtx,
};
use crate::error::MailboxError;
use crate::policy::{ConnRateLimiter, GlobalRateLimiter, Policy};
use crate::store::Store;

/// Runtime handle for the mailbox server. The library does NOT own
/// Arti; the binary wraps `MailboxServer` and pipes inbound streams
/// into [`accept_loop`].
#[derive(Debug)]
pub struct MailboxServer {
    store: Arc<Store>,
    policy: Policy,
    challenges: Arc<Mutex<Challenges>>,
    global_rl: GlobalRateLimiter,
}

impl MailboxServer {
    /// Construct a server. Background tasks are launched separately
    /// (Task 12); this constructor only sets up the request-path
    /// state the accept loop needs.
    pub fn new(store: Arc<Store>, policy: Policy) -> Self {
        let now_secs_f = wall_clock_secs_f();
        Self {
            store,
            policy: policy.clone(),
            challenges: Arc::new(Mutex::new(Challenges::default())),
            global_rl: GlobalRateLimiter::from_policy(&policy, now_secs_f),
        }
    }

    /// Get a handle to the shared [`Store`] (used by Task 12 background tasks).
    #[must_use]
    pub fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    /// Get a handle to the shared challenges table (Task 12 sweep).
    #[must_use]
    pub fn challenges(&self) -> Arc<Mutex<Challenges>> {
        self.challenges.clone()
    }

    /// Drive one client stream until it closes or errors. Per the spec
    /// the connection is **not** closed on policy / auth / rate-limit
    /// rejections; only frame-level codec errors or I/O errors close it.
    pub async fn accept_loop<S>(&self, stream: S) -> Result<(), MailboxError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut framed = Framed::new(stream, MailboxFrameCodec::new());
        let conn_rl = Mutex::new(ConnRateLimiter::from_policy(
            &self.policy,
            wall_clock_secs_f(),
        ));

        let idle = std::time::Duration::from_secs(u64::from(self.policy.idle_timeout_secs));
        loop {
            // Bounds the silence *between* frames (not connection lifetime): the
            // protocol is request/reply, so a client with no reason to pause past
            // `idle_timeout_secs` is shed. Re-armed each read.
            let next = match tokio::time::timeout(idle, framed.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(()),
                Err(_elapsed) => {
                    debug!("closing idle connection (idle_timeout)");
                    return Ok(());
                }
            };
            let frame = match next {
                Ok(f) => f,
                Err(e @ MailboxError::Transport(_)) => {
                    // Codec-level malformed → reply Error frame, keep
                    // the connection open. `Framed` latches a
                    // `has_errored` flag after a `Decoder::decode`
                    // error; we rebuild the wrapper so the next
                    // `poll_next` doesn't immediately return `None`.
                    // Any unread bytes still buffered are abandoned —
                    // the protocol does not pipeline requests, so a
                    // well-behaved client only sends the next frame
                    // after seeing our `Error` reply.
                    let resp = error_frame(&e);
                    if framed.send(resp).await.is_err() {
                        return Ok(());
                    }
                    let inner = framed.into_inner();
                    framed = Framed::new(inner, MailboxFrameCodec::new());
                    continue;
                }
                Err(other) => {
                    debug!(?other, "stream-level error closing accept_loop");
                    return Err(other);
                }
            };

            let now = wall_clock_secs_i64();
            let now_f = wall_clock_secs_f();
            let ctx = DispatchCtx {
                store: &self.store,
                policy: &self.policy,
                challenges: &self.challenges,
                conn_rl: &conn_rl,
                global_rl: &self.global_rl,
            };

            let resp = match frame {
                MailboxFrame::Deposit(b) => handle_deposit(&ctx, b, now, now_f),
                MailboxFrame::Challenge(b) => handle_challenge(&ctx, b, now),
                MailboxFrame::Fetch(b) => handle_fetch(&ctx, b, now, now_f),
                MailboxFrame::Delete(b) => handle_delete(&ctx, b, now),
                // S→C frames or unexpected client frames just yield a
                // MalformedRequest reply.
                _ => Err(MailboxError::Transport(
                    crate::error::TransportErrorKind::DecodeFailed(
                        "unexpected client frame".into(),
                    ),
                )),
            };

            let to_send = match resp {
                Ok(f) => f,
                Err(ref e) => {
                    warn!(kind = ?e.kind(), "request rejected");
                    error_frame(e)
                }
            };

            if framed.send(to_send).await.is_err() {
                // Client hung up mid-write; drop quietly.
                return Ok(());
            }
        }
    }
}

fn wall_clock_secs_i64() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs() as i64
}

fn wall_clock_secs_f() -> f64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs_f64()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    use skattr_core::mailbox::protocol::{Challenge, Deposit, PROTOCOL_VERSION};

    use crate::codec::MailboxFrameCodec;

    #[tokio::test]
    async fn deposit_then_challenge_round_trip_via_duplex() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());

        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        // Drive the client side.
        let mut framed = Framed::new(client, MailboxFrameCodec::new());

        framed
            .send(MailboxFrame::Deposit(Deposit {
                version: PROTOCOL_VERSION,
                recipient_hash: [0xAB; 32],
                ciphertext: vec![1, 2, 3, 4],
                ttl_request: 86_400,
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::DepositOk(_)));

        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed); // close client → server loop returns
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rate_limit_does_not_close_connection() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mut policy = Policy::recommended();
        policy.per_conn_deposits_per_min = 1; // hit limit immediately
        let mb = MailboxServer::new(store, policy);
        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        let mut framed = Framed::new(client, MailboxFrameCodec::new());
        for i in 0..3 {
            framed
                .send(MailboxFrame::Deposit(Deposit {
                    version: PROTOCOL_VERSION,
                    recipient_hash: [i; 32],
                    ciphertext: vec![1, 2, 3],
                    ttl_request: 86_400,
                }))
                .await
                .unwrap();
            let resp = framed.next().await.unwrap().unwrap();
            // First Ok, second + third Error::RateLimited; in any case the connection stays open.
            let _ = resp;
        }
        // Connection still alive: send a Challenge.
        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0xCD; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed);
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn idle_connection_is_closed_after_timeout() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mut policy = Policy::recommended();
        policy.idle_timeout_secs = 1; // close after 1s of silence
        let mb = MailboxServer::new(store, policy);
        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        // Client sends nothing. The idle timeout must fire and close the conn.
        // Reading the client side should observe EOF within a few seconds.
        let mut framed = Framed::new(client, MailboxFrameCodec::new());
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), framed.next()).await;
        match next {
            Ok(None) => {} // server closed (EOF) — pass
            Ok(Some(_)) => panic!("unexpected frame from idle server"),
            Err(_) => panic!("server did not close the idle connection within 5s"),
        }
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn unknown_frame_type_returns_error_keeps_connection_open() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let store = Arc::new(Store::in_memory().unwrap());
        let mb = MailboxServer::new(store, Policy::recommended());
        let server_task = tokio::spawn(async move { mb.accept_loop(server).await });

        // Hand-craft a frame with type byte 0x20 (unknown).
        use tokio::io::AsyncWriteExt;
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&[0x20]);
        client.write_all(&buf).await.unwrap();

        // Expect an Error frame back.
        let mut framed = Framed::new(client, MailboxFrameCodec::new());
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::Error(_)));

        // Still open — drive a valid Challenge.
        framed
            .send(MailboxFrame::Challenge(Challenge {
                version: PROTOCOL_VERSION,
                identity_hash: [0; 32],
            }))
            .await
            .unwrap();
        let resp = framed.next().await.unwrap().unwrap();
        assert!(matches!(resp, MailboxFrame::ChallengeNonce(_)));

        drop(framed);
        server_task.await.unwrap().unwrap();
    }
}
