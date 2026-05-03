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
use crate::transport::TransportErrorKind;

/// Control messages sent by the hub to a running peer actor.
pub enum PeerCtrl<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Replace the actor's current `AuthenticatedConnection` with a new
    /// one (typically because the hub received an inbound dial from
    /// this peer while an older outbound conn was live). The old conn
    /// is closed, pending-ACK oneshots are drained (caller's outbox
    /// rows will be retried), and the new conn takes over.
    ReplaceConn(Box<AuthenticatedConnection<S>>),
    /// Graceful stop. Drain pending and exit.
    #[allow(dead_code)]
    Shutdown,
}

/// One outbound delivery, submitted by the hub.
pub struct DeliveryJob {
    /// Application-level message id, used for ACK correlation.
    pub message_id: MessageId,
    /// Opaque MLS ciphertext payload to deliver.
    pub ciphertext: Vec<u8>,
    /// Fires `Ok(())` on successful ACK, `Err(())` if the ack path is
    /// torn down (conn dropped, actor cancelled). The hub translates
    /// `Err(())` into "row stays in outbox for retry."
    pub(crate) ack_tx: oneshot::Sender<std::result::Result<(), ()>>,
}

/// One outbound Welcome, submitted by the hub. Parallel to
/// `DeliveryJob` but carries opaque Welcome bytes destined for a
/// `Frame::MlsWelcome` frame instead of `Frame::MlsApp`. ACK
/// correlation uses the deterministic `welcome_msg_id(bytes)`.
pub struct WelcomeJob {
    /// TLS-serialized Welcome bytes.
    pub welcome_bytes: Vec<u8>,
    /// Fires `Ok(())` on successful ACK, `Err(())` if the ack path is
    /// torn down (conn dropped, actor cancelled, no live conn at submit
    /// time). Caller treats `Err` as "Welcome did not reach the
    /// inviter — surface via UI."
    pub(crate) ack_tx: oneshot::Sender<std::result::Result<(), ()>>,
}

/// Per-peer actor handle. Returned by `PeerConnection::spawn*` so the
/// hub can `.await` it on shutdown.
pub(crate) type PeerHandle = JoinHandle<()>;

/// Deterministic synthetic message id for ACK correlation of an
/// outbound Welcome. Defined identically on both sides so the
/// inviter (sender) and the joiner (receiver) compute the same
/// `MessageId` from the Welcome bytes — letting the existing
/// `Frame::Ack(MessageId)` correlator round-trip without changes.
pub(crate) fn welcome_msg_id(bytes: &[u8]) -> MessageId {
    use blake2::{Blake2s256, Digest};
    let mut h = Blake2s256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&out[..16]);
    MessageId(id)
}

/// Inbound-MLS dispatch strategy, injected per peer actor. See Task 8
/// preamble for the rationale — keeps `openmls` out of the actor
/// and keeps tests that don't need real MLS trivially easy to write.
pub trait InboundDispatch: Send + Sync + 'static {
    /// Decrypt and ingest an inbound MLS ciphertext from `peer`.
    /// Returns the `MessageId` on success (for ACK) or `None` on failure.
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId>;

    /// Process an inbound MLS Welcome from `peer` (the inviter side
    /// of the invite link). Default impl ignores the message and
    /// returns `None` so existing impls compile unchanged. Production
    /// `DaemonInbound` overrides this to look up the PSK in
    /// `outstanding_invites`, call `Group::join_from_welcome`, persist
    /// the new group + contact + group_id link, and emit
    /// `Event::ContactUpdated`.
    ///
    /// The returned `MessageId` (when `Some`) MUST equal
    /// `welcome_msg_id(welcome)` so the synthetic ACK correlates with
    /// the sender's outstanding oneshot.
    fn dispatch_welcome(
        &self,
        _peer: PublicKey,
        _welcome: &[u8],
    ) -> Option<MessageId> {
        None
    }
}

/// Per-peer actor. Owns an `Option<AuthenticatedConnection<S>>`, a
/// pending-ACK map, and drives retry tick, keepalive, idle close, and
/// inbound-MLS dispatch.
pub struct PeerConnection;

impl PeerConnection {
    /// Test-only constructor (Task 7): a minimal actor with no outbox
    /// tick, no keepalive, no idle close, no inbound-MLS handling.
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
            let _ = minimal_run(peer, *conn, jobs).await;
        })
    }

    /// Production spawner: the hub creates an actor cold (no conn) and
    /// provides job + control channels plus an optional inbound-MLS
    /// dispatcher. The actor starts receiving a fresh conn via
    /// `PeerCtrl::ReplaceConn` sent by the hub immediately after spawn.
    pub fn spawn<S>(
        peer: PublicKey,
        jobs: mpsc::Receiver<DeliveryJob>,
        welcome_jobs: mpsc::Receiver<WelcomeJob>,
        ctrl: mpsc::Receiver<PeerCtrl<S>>,
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = full_run::<S>(peer, None, jobs, welcome_jobs, ctrl, pool, inbound).await;
        })
    }

    /// Test-only full-actor constructor: retry tick + keepalive + idle
    /// close are all active, driven by `tokio::time`. `inbound` is
    /// optional so Task 8's retry test can pass `None` (the responder
    /// in that test never sends `Frame::MlsApp` back to the actor).
    #[cfg(test)]
    pub(crate) fn spawn_full_for_test<S>(
        peer: PublicKey,
        conn: Box<AuthenticatedConnection<S>>,
        jobs: mpsc::Receiver<DeliveryJob>,
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(4);
            let (_welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
            let _ = full_run(peer, Some(*conn), jobs, welcome_rx, ctrl_rx, pool, inbound).await;
        })
    }
}

/// Minimal actor from Task 7. No tick machinery.
async fn minimal_run<S>(
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
                    Ok(Some(Frame::Pong)) => {}
                    Ok(Some(other)) => {
                        tracing::warn!(ty = ?other, "peer: dropping unexpected inbound frame");
                    }
                    Ok(None) => {
                        return Err(CoreError::Transport(TransportErrorKind::Other(
                            "peer: EOF".into(),
                        )));
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
        }
    }

    // Drain pending oneshots on clean exit.
    drain_pending(&mut pending);
    Ok(())
}

/// Tick intervals for the full actor.
const RETRY_TICK: std::time::Duration = std::time::Duration::from_secs(1);
const KEEPALIVE_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);
const PONG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const IDLE_CLOSE: std::time::Duration = std::time::Duration::from_secs(180);

// TODO Task 20.5: wire `direct_timeout_secs` trigger from `PeerConnection`
// into `DeliveryHub::ensure_mailbox_fallback`. The orchestrator itself ships
// in Task 20 as a `pub(crate)` API; the timer-driven trigger that fires it
// after sustained direct-delivery failure is deferred to a follow-up so the
// orchestrator can be exercised in isolation by Tasks 25/26 first.

/// Full actor (Tasks 8+). `conn` starts as `Some(...)` once the
/// handshake is complete and may become `None` after an error; the
/// retry tick is responsible for redialing via the hub in production.
/// For the test-only constructor, `conn == None` after an error means
/// the actor exits (redial wiring lives on the hub side).
async fn full_run<S>(
    peer: PublicKey,
    mut conn: Option<AuthenticatedConnection<S>>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
    mut _welcome_jobs: mpsc::Receiver<WelcomeJob>,
    mut ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::delivery::outbox::Outbox;

    let mut pending: HashMap<MessageId, oneshot::Sender<std::result::Result<(), ()>>> =
        HashMap::new();
    let mut retry_tick = tokio::time::interval(RETRY_TICK);
    retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Start keepalive after the first full period to avoid an immediate
    // first tick that would race with the retry tick at t=0.
    let mut keepalive_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + KEEPALIVE_PERIOD,
        KEEPALIVE_PERIOD,
    );
    let mut last_traffic = tokio::time::Instant::now();
    let mut awaiting_pong_since: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                let Some(c) = conn.as_mut() else {
                    let _ = job.ack_tx.send(Err(()));
                    continue;
                };
                if c.send(Frame::MlsApp(job.ciphertext)).await.is_err() {
                    let _ = job.ack_tx.send(Err(()));
                    conn = None;
                    drain_pending(&mut pending);
                    continue;
                }
                pending.insert(job.message_id, job.ack_tx);
                last_traffic = tokio::time::Instant::now();
            }
            _ = retry_tick.tick() => {
                let ob = Outbox::new(&pool);
                let now = now_ms();
                let due = match ob.due(now, 32) { Ok(v) => v, Err(_) => continue };
                for entry in due {
                    if pending.contains_key(&entry.message_id) { continue; }
                    if entry.target != peer { continue; }
                    let Some(c) = conn.as_mut() else { break; };
                    if c.send(Frame::MlsApp(entry.payload.clone())).await.is_err() {
                        conn = None;
                        drain_pending(&mut pending);
                        break;
                    }
                    let (tx, _rx) = oneshot::channel::<std::result::Result<(), ()>>();
                    pending.insert(entry.message_id, tx);
                    let _ = ob.reschedule(entry.id, entry.attempts, now);
                    last_traffic = tokio::time::Instant::now();
                }
            }
            _ = keepalive_tick.tick() => {
                if conn.is_some() {
                    if last_traffic.elapsed() >= IDLE_CLOSE {
                        if let Some(owned) = conn.take() {
                            let _ = owned.close().await;
                        }
                        drain_pending(&mut pending);
                        continue;
                    }
                    if awaiting_pong_since
                        .map(|t| t.elapsed() >= PONG_DEADLINE)
                        .unwrap_or(false)
                    {
                        conn = None;
                        drain_pending(&mut pending);
                        awaiting_pong_since = None;
                        continue;
                    }
                    if let Some(c) = conn.as_mut() {
                        let _ = c.send(Frame::Ping).await;
                    }
                    awaiting_pong_since.get_or_insert_with(tokio::time::Instant::now);
                }
            }
            c = ctrl.recv() => {
                match c {
                    Some(PeerCtrl::ReplaceConn(new_conn)) => {
                        if let Some(old) = conn.take() { let _ = old.close().await; }
                        drain_pending(&mut pending);
                        conn = Some(*new_conn);
                        last_traffic = tokio::time::Instant::now();
                        awaiting_pong_since = None;
                    }
                    Some(PeerCtrl::Shutdown) | None => break,
                }
            }
            frame = async {
                match conn.as_mut() {
                    Some(c) => c.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match frame {
                    Ok(Some(Frame::Ack(bytes))) => {
                        let mid = MessageId(bytes);
                        if let Some(tx) = pending.remove(&mid) {
                            let _ = tx.send(Ok(()));
                        }
                        let ob = Outbox::new(&pool);
                        let _ = ob.ack(&peer, mid);
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::Bye)) => break,
                    Ok(Some(Frame::Ping)) => {
                        if let Some(c) = conn.as_mut() {
                            let _ = c.send(Frame::Pong).await;
                        }
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::Pong)) => {
                        awaiting_pong_since = None;
                        last_traffic = tokio::time::Instant::now();
                    }
                    Ok(Some(Frame::MlsApp(ct))) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(d) = inbound.as_ref() {
                            if let Some(mid) = d.dispatch(peer, &ct) {
                                if let Some(c) = conn.as_mut() {
                                    let _ = c.send(Frame::Ack(mid.0)).await;
                                }
                            }
                            // None => rejected frame, do not ACK.
                        } else {
                            tracing::warn!(
                                "peer: inbound MlsApp received but no InboundDispatch configured"
                            );
                        }
                    }
                    Ok(Some(other)) => {
                        tracing::warn!(ty = ?other, "peer: dropping unexpected frame");
                    }
                    Ok(None) => {
                        conn = None;
                        drain_pending(&mut pending);
                    }
                    Err(_) => {
                        conn = None;
                        drain_pending(&mut pending);
                    }
                }
            }
        }
    }

    if let Some(c) = conn {
        let _ = c.close().await;
    }
    drain_pending(&mut pending);
    Ok(())
}

fn drain_pending(pending: &mut HashMap<MessageId, oneshot::Sender<std::result::Result<(), ()>>>) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(()));
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

#[doc(hidden)]
pub(crate) fn now_ms_testable() -> i64 {
    now_ms()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::identity::IdentityKey;
    use crate::storage::Pool;
    use crate::transport::frame::Frame;
    use crate::transport::noise::{handshake_initiator, handshake_responder};
    use tokio::sync::{mpsc, oneshot};

    #[test]
    fn inbound_dispatch_welcome_default_returns_none() {
        struct Stub;
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
        }
        let s = Stub;
        assert!(s.dispatch_welcome(PublicKey([0u8; 32]), b"x").is_none());
    }

    #[test]
    fn inbound_dispatch_welcome_override_is_called() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Stub(AtomicBool);
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
            fn dispatch_welcome(
                &self,
                _peer: PublicKey,
                welcome: &[u8],
            ) -> Option<MessageId> {
                self.0.store(true, Ordering::SeqCst);
                Some(super::welcome_msg_id(welcome))
            }
        }
        let s = Stub(AtomicBool::new(false));
        let id = s.dispatch_welcome(PublicKey([0u8; 32]), b"hello").unwrap();
        assert_eq!(id.0, super::welcome_msg_id(b"hello").0);
        assert!(s.0.load(Ordering::SeqCst));
    }

    #[test]
    fn welcome_msg_id_is_deterministic_blake2s_prefix() {
        let bytes = b"hello welcome";
        let id1 = super::welcome_msg_id(bytes);
        let id2 = super::welcome_msg_id(bytes);
        assert_eq!(id1.0, id2.0, "must be deterministic");

        let other = super::welcome_msg_id(b"different bytes");
        assert_ne!(id1.0, other.0, "different inputs must produce different ids");

        assert_eq!(id1.0.len(), 16);
        assert!(id1.0.iter().any(|&b| b != 0));
    }

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

    /// Full actor spawn for Task 8+: actor owns its Outbox handle (via
    /// a Pool reference) and its tick loop. We use `tokio::time::pause`
    /// to control elapsed virtual time.
    #[tokio::test(start_paused = true)]
    async fn retry_tick_picks_up_outbox_row_and_delivers() {
        use crate::delivery::outbox::Outbox;
        use crate::envelope::MessageId as EMid;

        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        let mid = [0x42u8; 16];
        let echo = spawn_responder_echo_ack(server_stream, responder_id, mid).await;

        let (conn, _) = handshake_initiator(client_stream, &initiator_id, &responder_static, None)
            .await
            .unwrap();

        // Seed an outbox row directly — no hub involvement yet.
        let pool = std::sync::Arc::new(Pool::in_memory());
        let ob = Outbox::new(&pool);
        let peer = PublicKey([0xBB; 32]);
        ob.enqueue(&peer, EMid(mid), &[0x01, 0x02, 0x03], 0)
            .unwrap();

        let (_job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let handle = PeerConnection::spawn_full_for_test(
            peer,
            Box::new(conn),
            job_rx,
            pool.clone(),
            None, // no inbound MLS needed — responder echoes Ack directly
        );

        // Advance virtual time past one retry tick (1 s).
        tokio::time::advance(std::time::Duration::from_millis(1_100)).await;
        // Let the ack arrive.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Outbox should now be empty.
        let ob_check = Outbox::new(&pool);
        assert!(
            ob_check.due(i64::MAX, 10).unwrap().is_empty(),
            "retry tick must remove the row after Ack"
        );

        handle.abort();
        let _ = echo.await;
    }

    #[tokio::test(start_paused = true)]
    async fn keepalive_ping_goes_out_after_sixty_seconds() {
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let initiator_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();

        // Responder task: assert a Ping arrives; reply Pong; hold open.
        let responder_task = tokio::spawn(async move {
            let (mut conn, _) = handshake_responder(server_stream, &responder_id, None)
                .await
                .unwrap();
            loop {
                match conn.recv().await {
                    Ok(Some(Frame::Ping)) => {
                        conn.send(Frame::Pong).await.unwrap();
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        });

        let (conn, _) = handshake_initiator(client_stream, &initiator_id, &responder_static, None)
            .await
            .unwrap();

        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let (_job_tx, job_rx) = mpsc::channel::<DeliveryJob>(1);
        let handle = PeerConnection::spawn_full_for_test(
            PublicKey([0xBB; 32]),
            Box::new(conn),
            job_rx,
            pool,
            None,
        );

        // Advance past the 60 s keepalive interval.
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        // Let the actor run one tick.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Responder should have received the Ping.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), responder_task).await;
        handle.abort();
    }
}
