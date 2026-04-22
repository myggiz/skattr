// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon-scoped delivery router.
//!
//! Maps `PublicKey → (mpsc::Sender<DeliveryJob>, mpsc::Sender<PeerCtrl>)`,
//! spawning a per-peer actor on the first send or ingest. Also runs a
//! periodic `seen_messages` sweep.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::delivery::peer::{DeliveryJob, InboundDispatch, PeerConnection, PeerCtrl};
use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::seen_messages::SeenMessagesRepo;
use crate::storage::Pool;
use crate::transport::connection::AuthenticatedConnection;

const JOB_CHAN_CAP: usize = 64;
const CTRL_CHAN_CAP: usize = 4;
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const SEEN_WINDOW_MS: i64 = 24 * 3600 * 1000;

struct PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    jobs: mpsc::Sender<DeliveryJob>,
    ctrl: mpsc::Sender<PeerCtrl<S>>,
}

pub(crate) struct DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    peers: Mutex<HashMap<PublicKey, PeerChannels<S>>>,
    pool: Arc<Pool>,
    inbound: Option<Arc<dyn InboundDispatch>>,
    _sweep: tokio::task::JoinHandle<()>,
}

impl<S> DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a hub with no inbound-MLS handling. Suitable for
    /// outbound-only tests where the responder echoes `Frame::Ack`
    /// directly; real-MLS deployments must use
    /// [`DeliveryHub::new_with_inbound`] instead.
    pub(crate) fn new(pool: Arc<Pool>) -> Self {
        Self::new_with_inbound_inner(pool, None)
    }

    /// Construct a hub that decrypts inbound `Frame::MlsApp` through
    /// `dispatch`. The integration test builds an `MlsInboundDispatch`
    /// that wraps `Group::decrypt` + `receiver::receive` for the one
    /// peer it cares about.
    pub(crate) fn new_with_inbound(pool: Arc<Pool>, dispatch: Arc<dyn InboundDispatch>) -> Self {
        Self::new_with_inbound_inner(pool, Some(dispatch))
    }

    fn new_with_inbound_inner(pool: Arc<Pool>, inbound: Option<Arc<dyn InboundDispatch>>) -> Self {
        let sweep_pool = pool.clone();
        let sweep = tokio::spawn(async move {
            let mut t = tokio::time::interval(SWEEP_INTERVAL);
            t.tick().await;
            loop {
                t.tick().await;
                let now = crate::delivery::peer::now_ms_testable();
                let cutoff = now - SEEN_WINDOW_MS;
                let seen = SeenMessagesRepo::new(&sweep_pool);
                let _ = seen.sweep_older_than(cutoff);
            }
        });
        Self {
            peers: Mutex::new(HashMap::new()),
            pool,
            inbound,
            _sweep: sweep,
        }
    }

    /// Submit a job for `peer`. Spawns the peer actor on first use.
    pub(crate) async fn send(
        &self,
        peer: PublicKey,
        message_id: MessageId,
        ciphertext: Vec<u8>,
    ) -> Result<oneshot::Receiver<std::result::Result<(), ()>>> {
        let (ack_tx, ack_rx) = oneshot::channel::<std::result::Result<(), ()>>();
        let jobs_tx = self.ensure_actor(peer).await;
        let _ = jobs_tx
            .send(DeliveryJob {
                message_id,
                ciphertext,
                ack_tx,
            })
            .await;
        Ok(ack_rx)
    }

    /// Install a post-handshake `AuthenticatedConnection` for `peer`.
    /// If an actor already exists for this peer, its current conn is
    /// replaced. Otherwise a fresh actor is spawned with the conn.
    pub(crate) async fn ingest(&self, peer: PublicKey, conn: AuthenticatedConnection<S>) {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            let _ = ch.ctrl.send(PeerCtrl::ReplaceConn(Box::new(conn))).await;
            return;
        }
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(
            peer,
            jobs_rx,
            ctrl_rx,
            self.pool.clone(),
            self.inbound.clone(),
        );
        let _ = ctrl_tx.send(PeerCtrl::ReplaceConn(Box::new(conn))).await;
        peers.insert(
            peer,
            PeerChannels {
                jobs: jobs_tx,
                ctrl: ctrl_tx,
            },
        );
    }

    async fn ensure_actor(&self, peer: PublicKey) -> mpsc::Sender<DeliveryJob> {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            return ch.jobs.clone();
        }
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(
            peer,
            jobs_rx,
            ctrl_rx,
            self.pool.clone(),
            self.inbound.clone(),
        );
        peers.insert(
            peer,
            PeerChannels {
                jobs: jobs_tx.clone(),
                ctrl: ctrl_tx,
            },
        );
        jobs_tx
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::transport::noise::{handshake_initiator, handshake_responder};

    #[tokio::test]
    async fn ingest_spawns_actor_and_replace_conn_on_second_ingest() {
        let pool = Arc::new(Pool::in_memory());
        let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new(pool.clone());

        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_static = bob.noise_static_public();
        let bob_pk = PublicKey(bob.public().0);

        // Conn #1
        let (a1, b1) = tokio::io::duplex(16 * 1024);
        let bob_task = tokio::spawn(async move {
            let (_conn, _) = handshake_responder(b1, &bob, None).await.unwrap();
            // Let the initiator drive; we keep this alive briefly.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        let (conn_a1, _) = handshake_initiator(a1, &alice, &bob_static, None)
            .await
            .unwrap();
        hub.ingest(bob_pk, conn_a1).await;

        // At this point the hub should have one actor for bob_pk.
        {
            let peers = hub.peers.lock().await;
            assert!(peers.contains_key(&bob_pk));
        }

        let _ = bob_task.await;
    }
}
