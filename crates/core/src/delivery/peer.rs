// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-peer connection actor.

use std::collections::HashMap;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::transport::connection::AuthenticatedConnection;
use crate::transport::frame::Frame;

/// One outbound delivery, submitted by the hub.
pub(crate) struct DeliveryJob {
    pub(crate) message_id: MessageId,
    pub(crate) ciphertext: Vec<u8>,
    /// Fires `Ok(())` on successful ACK, `Err(())` if the ack path is
    /// torn down (conn dropped, actor cancelled). The hub translates
    /// `Err(())` into "row stays in outbox for retry."
    pub(crate) ack_tx: oneshot::Sender<std::result::Result<(), ()>>,
}

/// Per-peer actor handle. Returned by `PeerConnection::spawn*` so the
/// hub can `.await` it on shutdown.
pub(crate) type PeerHandle = JoinHandle<()>;

/// Minimal "happy-path" actor. Owns a single `AuthenticatedConnection`,
/// a pending-ACK map, and a `select!` over job intake + frame recv.
///
/// Task 8 extends this with retry tick, keepalive, and idle close.
/// Task 9 extends it with `ReplaceConn` support.
pub(crate) struct PeerConnection;

impl PeerConnection {
    /// Test-only constructor: hand in an already-dialed connection and
    /// an already-opened job receiver. The actor runs until the job
    /// receiver closes or the connection errors.
    #[cfg(test)]
    pub(crate) fn spawn_with_conn_for_test<S>(
        peer: PublicKey,
        conn: Box<AuthenticatedConnection<S>>,
        jobs: mpsc::Receiver<DeliveryJob>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = run_actor(peer, *conn, jobs).await;
        })
    }
}

async fn run_actor<S>(
    _peer: PublicKey,
    mut conn: AuthenticatedConnection<S>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut pending: HashMap<MessageId, oneshot::Sender<std::result::Result<(), ()>>> =
        HashMap::new();

    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                if let Err(e) = conn.send(Frame::MlsApp(job.ciphertext)).await {
                    let _ = job.ack_tx.send(Err(()));
                    return Err(e);
                }
                pending.insert(job.message_id, job.ack_tx);
            }
            frame = conn.recv() => {
                match frame {
                    Ok(Some(Frame::Ack(bytes))) => {
                        let mid = MessageId(bytes);
                        if let Some(tx) = pending.remove(&mid) {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    Ok(Some(Frame::Bye)) => {
                        break;
                    }
                    Ok(Some(Frame::Ping)) => {
                        let _ = conn.send(Frame::Pong).await;
                    }
                    Ok(Some(Frame::Pong)) => { /* handled by keepalive in Task 8 */ }
                    Ok(Some(other)) => {
                        tracing::warn!(ty = ?other, "peer: dropping unexpected inbound frame");
                    }
                    Ok(None) => {
                        return Err(CoreError::Transport("peer: EOF".into()));
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
        }
    }

    // Drain pending oneshots on clean exit.
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::transport::frame::Frame;
    use crate::transport::noise::{handshake_initiator, handshake_responder};
    use tokio::sync::{mpsc, oneshot};

    /// Spawn a matching responder task over one half of a duplex pair.
    /// Returns a join handle that resolves when the responder observes
    /// one `MlsApp` ciphertext and echoes back an `Ack(mid)` frame.
    async fn spawn_responder_echo_ack(
        stream: tokio::io::DuplexStream,
        responder_identity: IdentityKey,
        expected_mid: [u8; 16],
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (mut conn, _outcome) = handshake_responder(stream, &responder_identity, None)
                .await
                .unwrap();
            // One frame expected.
            let frame = conn.recv().await.unwrap().expect("frame");
            match frame {
                Frame::MlsApp(_) => {}
                other => panic!("expected MlsApp, got {other:?}"),
            }
            conn.send(Frame::Ack(expected_mid)).await.unwrap();
        })
    }

    #[tokio::test]
    async fn actor_sends_mlsapp_and_resolves_oneshot_on_ack() {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        let mid = [0xA5u8; 16];
        let echo = spawn_responder_echo_ack(server_stream, responder_id, mid).await;

        // Initiator-side handshake in the actor's place: build a conn up
        // front and hand it to the actor via its test-only constructor.
        let (conn, _) = handshake_initiator(client_stream, &initiator_id, &responder_static, None)
            .await
            .unwrap();

        let (job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let (ack_tx, ack_rx) = oneshot::channel::<std::result::Result<(), ()>>();
        let handle =
            PeerConnection::spawn_with_conn_for_test(PublicKey([0xBB; 32]), Box::new(conn), job_rx);

        job_tx
            .send(DeliveryJob {
                message_id: crate::envelope::MessageId(mid),
                ciphertext: vec![0xDE, 0xAD, 0xBE, 0xEF],
                ack_tx,
            })
            .await
            .unwrap();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx)
            .await
            .expect("oneshot must fire within 2s")
            .expect("sender side not dropped");
        assert!(outcome.is_ok(), "happy path delivers");

        drop(job_tx);
        let _ = echo.await;
        let _ = handle.await;
    }
}
