// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Inbound accept loop: handshake each inbound stream, resolve the peer, and
//! ingest authorized connections into the DeliveryHub. Unknown peers are
//! rejected before ingest.

use crate::delivery::hub::DeliveryHub;
use crate::delivery::peer::InboundDispatch;
use crate::identity::IdentityKey;
use crate::storage::{ContactRepo, Pool};
use crate::transport::{handshake_responder, InboundStreams};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Bound on how long an unknown peer has to present its single first-contact
/// frame before we reject the connection (ADR 0007 carve-out).
const WELCOME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Drive the inbound stream source until it closes. Each accepted stream is
/// handshaked + resolved on its own task so a slow handshake can't stall the
/// loop. Returns when `inbound` is exhausted (transport shut down).
pub(crate) async fn run_accept_loop<S>(
    mut inbound: InboundStreams<S>,
    identity: Arc<IdentityKey>,
    pool: Arc<Pool>,
    hub: Arc<DeliveryHub<S>>,
    inbound_dispatch: Arc<dyn InboundDispatch>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while let Some(stream) = inbound.recv().await {
        let identity = identity.clone();
        let pool = pool.clone();
        let hub = hub.clone();
        let inbound_dispatch = inbound_dispatch.clone();
        // TODO(phase-2 transport hardening): each inbound stream spawns a
        // detached handshake task. It's onion-gated and 30s-timeout-bounded, but
        // an attacker who knows the onion could open many connections at once.
        // Bound concurrency with a Semaphore permit acquired before spawn, and
        // track tasks (JoinSet) so shutdown can drain them. Mirrors the mailbox
        // server's per-conn/global token buckets (Phase 2.A).
        tokio::spawn(async move {
            let (mut conn, outcome) = match handshake_responder(stream, &identity, None).await {
                Ok(v) => v,
                Err(e) => {
                    // `e` is a `TransportErrorKind`-backed `CoreError`. The
                    // responder never learns the inbound peer's onion, so its
                    // Display carries only static handshake/timeout text — no
                    // onion or pubkey to leak.
                    tracing::warn!(error = %e, "accept: inbound handshake failed");
                    return;
                }
            };
            match ContactRepo::new(&pool).find_by_noise_x25519(&outcome.peer_x25519) {
                Ok(Some(peer)) => hub.ingest(peer, conn).await,
                Ok(None) => {
                    // Unknown peer: ADR 0007 carve-out. Allow EXACTLY ONE frame
                    // under a bounded timeout. If it is a first-contact Welcome,
                    // authenticate + bind + join it; otherwise reject. Nothing
                    // else from an unknown peer reaches the MLS app pipeline.
                    match tokio::time::timeout(WELCOME_READ_TIMEOUT, conn.recv()).await {
                        Ok(Ok(Some(crate::transport::Frame::MlsWelcome(bytes)))) => {
                            match inbound_dispatch
                                .dispatch_welcome_bootstrap(&bytes, &outcome.peer_x25519)
                            {
                                Some(peer) => {
                                    // ACK so the inviter-side WelcomeJob resolves,
                                    // then ingest under the derived, binding-
                                    // verified peer so subsequent app frames flow.
                                    let ack_id = crate::delivery::peer::welcome_msg_id(&bytes);
                                    let _ = conn.send(crate::transport::Frame::Ack(ack_id.0)).await;
                                    hub.ingest(peer, conn).await;
                                }
                                None => {
                                    tracing::warn!(
                                        "accept: rejected first-contact welcome (invalid or unbound)"
                                    );
                                    let _ = conn.close().await;
                                }
                            }
                        }
                        _ => {
                            tracing::warn!("accept: rejected inbound connection from unknown peer");
                            let _ = conn.close().await;
                        }
                    }
                }
                Err(e) => {
                    // Storage error Display: static SQL/`StorageErrorKind` text,
                    // onion- and secret-free.
                    tracing::warn!(error = %e, "accept: peer resolution failed");
                    let _ = conn.close().await;
                }
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contact::Contact;
    use crate::envelope::MessageId;
    use crate::identity::PublicKey;
    use crate::transport::handshake_initiator;
    use tokio::sync::mpsc;

    /// Trivial no-op dispatch: never bootstraps a Welcome, never decrypts.
    /// Keeps the accept-loop unit tests independent of MLS/storage.
    struct NoopDispatch;
    impl InboundDispatch for NoopDispatch {
        fn dispatch(&self, _peer: PublicKey, _ciphertext: &[u8]) -> Option<MessageId> {
            None
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accept_rejects_unknown_peer() {
        let pool = Arc::new(Pool::in_memory()); // no contacts -> unknown
        let me = Arc::new(IdentityKey::generate().unwrap());
        let hub = Arc::new(DeliveryHub::<tokio::io::DuplexStream>::new(pool.clone()));

        let (tx, rx) = mpsc::channel::<tokio::io::DuplexStream>(4);
        let inbound = InboundStreams(rx);
        let loop_task = tokio::spawn(run_accept_loop(
            inbound,
            me.clone(),
            pool.clone(),
            hub.clone(),
            Arc::new(NoopDispatch) as Arc<dyn InboundDispatch>,
        ));

        // The initiator must address the responder by ITS noise static pub.
        let me_x = me.noise_static_public();
        let (cli, srv) = tokio::io::duplex(64 * 1024);
        tx.send(srv).await.unwrap();
        let stranger = IdentityKey::generate().unwrap();
        let stranger_pub = stranger.public();
        // Initiator completes the handshake (responder runs inside the loop),
        // then DROPS the connection without sending any first frame. The
        // carve-out's bounded read therefore sees the conn close promptly
        // (Ok(None)/Err) rather than waiting out WELCOME_READ_TIMEOUT — the
        // unknown peer is rejected fast.
        let _ = handshake_initiator(cli, &stranger, &me_x, None).await;

        // Let the spawned accept task run; assert the stranger got NO actor.
        // Poll briefly to avoid timing flakiness; assert it never appears.
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if hub.has_peer(&stranger_pub).await {
                break;
            }
        }
        assert!(
            !hub.has_peer(&stranger_pub).await,
            "unknown peer must not be ingested"
        );

        drop(tx); // close inbound -> loop ends
        let _ = loop_task.await;
    }

    /// Companion direction: a *known* peer (a contact resolvable by its noise
    /// x25519 static) IS ingested. This guards the resolve-and-ingest arm so
    /// `accept_rejects_unknown_peer` can't pass trivially (e.g. if ingest were
    /// removed entirely). The full end-to-end is the Task 8 guardrail.
    #[tokio::test(flavor = "multi_thread")]
    async fn accept_ingests_known_peer() {
        let pool = Arc::new(Pool::in_memory());
        let me = Arc::new(IdentityKey::generate().unwrap());
        let hub = Arc::new(DeliveryHub::<tokio::io::DuplexStream>::new(pool.clone()));

        let stranger = IdentityKey::generate().unwrap();
        let stranger_pub = stranger.public();
        // Seed the dialer as a known contact so find_by_noise_x25519 resolves.
        ContactRepo::new(&pool)
            .upsert(&Contact {
                identity: stranger_pub,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();

        let (tx, rx) = mpsc::channel::<tokio::io::DuplexStream>(4);
        let inbound = InboundStreams(rx);
        let loop_task = tokio::spawn(run_accept_loop(
            inbound,
            me.clone(),
            pool.clone(),
            hub.clone(),
            Arc::new(NoopDispatch) as Arc<dyn InboundDispatch>,
        ));

        let me_x = me.noise_static_public();
        let (cli, srv) = tokio::io::duplex(64 * 1024);
        tx.send(srv).await.unwrap();
        // Drive the initiator to completion; hold the conn open so the
        // responder-side actor stays registered for the assertion.
        let _held = handshake_initiator(cli, &stranger, &me_x, None)
            .await
            .map(|(conn, _)| conn);

        let mut ingested = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if hub.has_peer(&stranger_pub).await {
                ingested = true;
                break;
            }
        }
        assert!(ingested, "known peer must be ingested");

        drop(tx);
        let _ = loop_task.await;
        // Keep `_held` alive until here so the responder conn isn't torn down
        // before the assertion observes the actor.
        drop(_held);
    }
}
