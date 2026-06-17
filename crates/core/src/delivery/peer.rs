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
    fn dispatch_welcome(&self, _peer: PublicKey, _welcome: &[u8]) -> Option<MessageId> {
        None
    }

    /// Decrypt and ingest a mailbox-fetched MLS ciphertext whose sender is
    /// not known a priori. Implementations trial-decrypt against each known
    /// group, attribute the matching peer, persist, and emit
    /// `Event::MessageReceived`. Returns the `MessageId` on success (so the
    /// caller can server-side delete the deposit) or `None` on failure (the
    /// caller must NOT delete — the deposit is retried on the next poll).
    ///
    /// Default impl returns `None` so existing impls compile unchanged.
    fn dispatch_mailbox(&self, _ciphertext: &[u8]) -> Option<MessageId> {
        None
    }

    /// Authenticate + join a first-contact Welcome from a peer not yet known,
    /// deriving + binding the invitee's identity (ADR 0007). The Welcome is
    /// validated against `outstanding_invites` (peer-independent), the invitee
    /// identity is derived from the joined MLS group, and bound to the
    /// handshake's X25519 static (`expected_x25519`) before anything is
    /// persisted. `h_transport` is the handshake's transport↔MLS binding
    /// value (ADR 0009): when `Some`, the joiner registers it as the genesis
    /// commit's second external PSK and validates the binding while processing
    /// the Welcome's genesis Commit (active since 2.A Task 4). `None` means no
    /// transport binding is registered — used only by the established-conn
    /// `dispatch_welcome` path, which has no fresh handshake transcript. Typing
    /// it `Option` (rather than a zero-array sentinel) keeps a `[0u8; 32]` from
    /// ever masquerading as a real binding. Returns the derived peer
    /// [`PublicKey`] on success (so the accept loop can ingest under it), or
    /// `None` if the Welcome is invalid or fails the identity binding.
    ///
    /// Default impl returns `None` so existing impls compile unchanged.
    fn dispatch_welcome_bootstrap(
        &self,
        _welcome: &[u8],
        _expected_x25519: &[u8; 32],
        _h_transport: Option<&[u8; 32]>,
    ) -> Option<PublicKey> {
        None
    }

    /// Pop a queued inbound-attachment begin-request for `peer`, stashed when a
    /// `Kind::File` manifest was decrypted in `dispatch`. The actor calls this
    /// immediately after `dispatch` returns to learn it should start fetching.
    /// Default returns `None`.
    fn take_begin_attachment(
        &self,
        _peer: PublicKey,
    ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
        None
    }

    /// Emit `Event::AttachmentReceived`. Default no-op.
    fn attachment_received(
        &self,
        _peer: PublicKey,
        _attachment_id: [u8; 16],
        _filename: &str,
        _mime: &str,
        _size: u64,
        _path: &str,
    ) {
    }

    /// Emit `Event::AttachmentProgress`. Default no-op.
    fn attachment_progress(&self, _attachment_id: [u8; 16], _received: u32, _total: u32) {}

    /// Emit `Event::AttachmentFailed`. Default no-op.
    fn attachment_failed(&self, _attachment_id: [u8; 16], _reason: &str) {}
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn<S>(
        peer: PublicKey,
        jobs: mpsc::Receiver<DeliveryJob>,
        welcome_jobs: mpsc::Receiver<WelcomeJob>,
        ctrl: mpsc::Receiver<PeerCtrl<S>>,
        pool: std::sync::Arc<crate::storage::Pool>,
        inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
        dialer: Option<std::sync::Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
        direct_timeout: std::time::Duration,
        fallback: Option<std::sync::Arc<crate::delivery::hub::MailboxFallbackShared>>,
    ) -> PeerHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tokio::spawn(async move {
            let _ = full_run::<S>(
                peer,
                None,
                jobs,
                welcome_jobs,
                ctrl,
                pool,
                inbound,
                dialer,
                direct_timeout,
                fallback,
            )
            .await;
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
            let _ = full_run(
                peer,
                Some(*conn),
                jobs,
                welcome_rx,
                ctrl_rx,
                pool,
                inbound,
                None,
                std::time::Duration::from_secs(0),
                None,
            )
            .await;
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

/// Full actor (Tasks 8+). `conn` starts as `Some(...)` once the
/// handshake is complete and may become `None` after an error; the
/// retry tick is responsible for redialing via the hub in production.
/// For the test-only constructor, `conn == None` after an error means
/// the actor exits (redial wiring lives on the hub side).
#[allow(clippy::too_many_arguments)]
async fn full_run<S>(
    peer: PublicKey,
    mut conn: Option<AuthenticatedConnection<S>>,
    mut jobs: mpsc::Receiver<DeliveryJob>,
    mut welcome_jobs: mpsc::Receiver<WelcomeJob>,
    mut ctrl: mpsc::Receiver<PeerCtrl<S>>,
    pool: std::sync::Arc<crate::storage::Pool>,
    inbound: Option<std::sync::Arc<dyn InboundDispatch>>,
    dialer: Option<std::sync::Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
    direct_timeout: std::time::Duration,
    fallback: Option<std::sync::Arc<crate::delivery::hub::MailboxFallbackShared>>,
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
    // Sustained-direct-failure timer (Task 20.5): armed on the first
    // failure, cleared on any success. When it has been armed for at least
    // `direct_timeout`, the retry tick hands the peer's pending direct rows
    // to the mailbox fallback. Inert when `fallback` is `None` or
    // `direct_timeout == 0`, so all existing call sites are unaffected.
    let mut first_failure_at: Option<tokio::time::Instant> = None;
    let fallback_enabled = fallback.is_some() && direct_timeout > std::time::Duration::ZERO;

    loop {
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                // Dial on demand: if we have no live conn, try the injected
                // dialer before dropping the job. A failed dial (or no dialer)
                // leaves the row in the outbox for the retry tick.
                if !ensure_conn::<S>(peer, &mut conn, &dialer).await {
                    let _ = job.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                }
                let Some(c) = conn.as_mut() else {
                    let _ = job.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                };
                if c.send(Frame::MlsApp(job.ciphertext)).await.is_err() {
                    let _ = job.ack_tx.send(Err(()));
                    conn = None;
                    drain_pending(&mut pending);
                    arm_failure(&mut first_failure_at);
                    continue;
                }
                pending.insert(job.message_id, job.ack_tx);
                last_traffic = tokio::time::Instant::now();
                first_failure_at = None;
            }
            wj = welcome_jobs.recv() => {
                let Some(wj) = wj else { break; };
                let synthetic_id = welcome_msg_id(&wj.welcome_bytes);
                if !ensure_conn::<S>(peer, &mut conn, &dialer).await {
                    let _ = wj.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                }
                let Some(c) = conn.as_mut() else {
                    let _ = wj.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                };
                if c.send(Frame::MlsWelcome(wj.welcome_bytes)).await.is_err() {
                    let _ = wj.ack_tx.send(Err(()));
                    conn = None;
                    drain_pending(&mut pending);
                    arm_failure(&mut first_failure_at);
                    continue;
                }
                pending.insert(synthetic_id, wj.ack_tx);
                last_traffic = tokio::time::Instant::now();
                first_failure_at = None;
            }
            _ = retry_tick.tick() => {
                // dial-on-demand happens on the next job send; timer-driven redial is Phase-2 fallback work.
                let ob = Outbox::new(&pool);
                let now = now_ms();
                let due = match ob.due(now, 32) { Ok(v) => v, Err(_) => continue };
                for entry in due {
                    if pending.contains_key(&entry.message_id) { continue; }
                    if entry.target != peer { continue; }
                    if entry.target_kind == crate::storage::outbox::OutboxTargetKind::Mailbox {
                        continue;
                    }
                    let Some(c) = conn.as_mut() else { break; };
                    if c.send(Frame::MlsApp(entry.payload.clone())).await.is_err() {
                        conn = None;
                        drain_pending(&mut pending);
                        arm_failure(&mut first_failure_at);
                        break;
                    }
                    let (tx, _rx) = oneshot::channel::<std::result::Result<(), ()>>();
                    pending.insert(entry.message_id, tx);
                    let _ = ob.reschedule(entry.id, entry.attempts, now);
                    last_traffic = tokio::time::Instant::now();
                    first_failure_at = None;
                }
                // Task 20.5: after sustained UNBROKEN direct failure, hand the
                // peer's pending direct rows to the mailbox fallback (which
                // retargets them to a mailbox + attempts the first deposit).
                // The sweeper owns all subsequent retries, so we disarm here.
                // No lock guard is held across the `.await` below.
                if fallback_enabled
                    && first_failure_at
                        .map(|t| t.elapsed() >= direct_timeout)
                        .unwrap_or(false)
                {
                    if let Some(shared) = fallback.as_ref() {
                        // `pending` is empty here: the timer is armed only while sends
                        // are failing (every successful send clears it; failures drain
                        // pending), so no in-flight row can be double-handled.
                        let ob = Outbox::new(&pool);
                        let now = now_ms();
                        if let Ok(due) = ob.due(now, 32) {
                            for e in due.into_iter().filter(|e| {
                                e.target == peer
                                    && e.target_kind
                                        == crate::storage::outbox::OutboxTargetKind::Direct
                            }) {
                                crate::delivery::hub::run_mailbox_fallback(
                                    &pool, shared, peer, e.message_id, e.payload,
                                )
                                .await;
                            }
                        }
                    }
                    first_failure_at = None; // disarm; sweeper owns subsequent retries
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
                        // A fresh conn arrived: the next send may succeed, so
                        // do not let stale failures from the old conn fire the
                        // fallback. (Re-armed on the next failure if it recurs.)
                        first_failure_at = None;
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
                    Ok(Some(Frame::MlsWelcome(welcome_bytes))) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(d) = inbound.as_ref() {
                            if let Some(synthetic_id) =
                                d.dispatch_welcome(peer, &welcome_bytes)
                            {
                                if let Some(c) = conn.as_mut() {
                                    let _ = c.send(Frame::Ack(synthetic_id.0)).await;
                                }
                            }
                        } else {
                            tracing::warn!(
                                "peer: inbound MlsWelcome received but no \
                                 InboundDispatch configured"
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

/// Ensure the actor has a live connection, dialing on demand via the
/// injected dialer when cold. Returns `true` if a connection is now
/// present, `false` if there is no dialer or the dial failed (the caller
/// then NACKs the job, leaving the row for the retry tick).
async fn ensure_conn<S>(
    peer: PublicKey,
    conn: &mut Option<AuthenticatedConnection<S>>,
    dialer: &Option<std::sync::Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if conn.is_some() {
        return true;
    }
    let Some(d) = dialer.as_ref() else {
        return false;
    };
    match d.dial(peer).await {
        Ok((c, _h_transport)) => {
            *conn = Some(c);
            true
        }
        Err(e) => {
            // The dial error string can embed the peer's onion address
            // (ArtiTransport formats `connect {onion}:{port}`), which must not
            // appear at info+. Keep the WARN redaction-safe; log the detail at
            // DEBUG, where onions are permitted.
            tracing::warn!("delivery: outbound dial failed");
            tracing::debug!(error = %e, "delivery: outbound dial failure detail");
            false
        }
    }
}

fn drain_pending(pending: &mut HashMap<MessageId, oneshot::Sender<std::result::Result<(), ()>>>) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(()));
    }
}

/// Arm the sustained-direct-failure timer on the first failure of a run of
/// failures. Idempotent: only records the first failure instant until cleared.
fn arm_failure(first_failure_at: &mut Option<tokio::time::Instant>) {
    if first_failure_at.is_none() {
        *first_failure_at = Some(tokio::time::Instant::now());
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
            fn dispatch_welcome(&self, _peer: PublicKey, welcome: &[u8]) -> Option<MessageId> {
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
        assert_ne!(
            id1.0, other.0,
            "different inputs must produce different ids"
        );

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

    /// Verify a WelcomeJob round-trips: actor emits Frame::MlsWelcome,
    /// the test responder ACKs with the synthetic id, the oneshot
    /// resolves Ok.
    #[tokio::test]
    async fn welcome_job_round_trips_via_frame_mls_welcome() {
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());

        let actor_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();
        let peer = PublicKey(responder_id.public().0);

        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);

        // Handshakes must run concurrently (both sides block until the
        // other responds). Run the responder in a spawned task and join.
        let responder_task = tokio::spawn(async move {
            handshake_responder(server_stream, &responder_id, None)
                .await
                .unwrap()
        });
        let (actor_conn, _) =
            handshake_initiator(client_stream, &actor_id, &responder_static, None)
                .await
                .unwrap();
        let (mut responder_conn, _) = responder_task.await.unwrap();

        let (_jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(4);
        let (welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<tokio::io::DuplexStream>>(4);

        let _h = tokio::spawn(async move {
            let _ = super::full_run::<tokio::io::DuplexStream>(
                peer,
                Some(actor_conn),
                jobs_rx,
                welcome_rx,
                ctrl_rx,
                pool,
                None,
                None,
                std::time::Duration::from_secs(0),
                None,
            )
            .await;
        });

        let welcome_bytes = b"fake welcome bytes".to_vec();
        let synthetic_id = super::welcome_msg_id(&welcome_bytes);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        welcome_tx
            .send(WelcomeJob {
                welcome_bytes: welcome_bytes.clone(),
                ack_tx,
            })
            .await
            .unwrap();

        match responder_conn.recv().await {
            Ok(Some(Frame::MlsWelcome(got))) => assert_eq!(got, welcome_bytes),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
        responder_conn
            .send(Frame::Ack(synthetic_id.0))
            .await
            .unwrap();

        match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
            Ok(Ok(Ok(()))) => {}
            other => panic!("expected ACK, got {other:?}"),
        }
    }

    /// The direct retry tick must skip `target_kind='mailbox'` outbox
    /// rows: those are routed via the mailbox fallback path, never sent
    /// as `Frame::MlsApp` over the direct peer connection.
    /// Task 20.5: sustained UNBROKEN direct-delivery failure must hand the
    /// peer's pending DIRECT outbox rows to the mailbox fallback, which
    /// retargets them to a mailbox and deposits. We construct `full_run`
    /// with NO conn and NO dialer (every send fails → timer arms), a short
    /// `direct_timeout`, and a `MailboxFallbackShared` whose factory accepts
    /// the deposit. After the timeout the direct row is no longer Direct
    /// (either deleted = deposited, or flipped to Mailbox).
    #[tokio::test]
    async fn sustained_failure_triggers_mailbox_fallback() {
        use crate::contact::card::{ContactCard, ContactCardBody};
        use crate::contact::Contact;
        use crate::identity::Signature;
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::poll::{MailboxConnectFactory, MailboxStream};
        use crate::mailbox::protocol::DepositOk;
        use crate::storage::outbox::{OutboxRepo, OutboxTargetKind};
        use crate::storage::ContactRepo;
        use futures::{SinkExt, StreamExt};
        use std::sync::Mutex as StdMutex;
        use tokio_util::codec::Framed;

        // Inline mailbox server: accept one Deposit, reply DepositOk.
        async fn deposit_ok_server(server: tokio::io::DuplexStream) {
            let mut framed = Framed::new(server, MailboxFrameCodec::new());
            if let Some(Ok(MailboxFrame::Deposit(_))) = framed.next().await {
                let _ = framed
                    .send(MailboxFrame::DepositOk(DepositOk {
                        deposit_id: [0xAB; 16],
                        expires_at: 9_999,
                    }))
                    .await;
            }
        }

        // Stub factory: hands out one pre-seeded duplex per onion.
        struct StubFactory {
            slots: StdMutex<Vec<tokio::io::DuplexStream>>,
        }
        #[async_trait::async_trait]
        impl MailboxConnectFactory for StubFactory {
            async fn connect(&self, onion: &str) -> Result<MailboxClient<Box<dyn MailboxStream>>> {
                let s = self.slots.lock().unwrap().pop();
                match s {
                    Some(s) => {
                        let boxed: Box<dyn MailboxStream> = Box::new(s);
                        Ok(MailboxClient::from_stream(onion.to_string(), boxed))
                    }
                    None => Err(crate::error::CoreError::MailboxClient(
                        crate::error::MailboxClientErrorKind::Unreachable,
                    )),
                }
            }
        }

        let pool = std::sync::Arc::new(Pool::in_memory());
        let peer = PublicKey([0x42; 32]);
        let mid = [0x11u8; 16];

        // Link a 'theirs' mailbox to the peer via a contact card listing it,
        // so MailboxRepo::list_for_contact(&peer) returns the onion.
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: peer,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts
            .put_card(&ContactCard {
                body: ContactCardBody {
                    identity: peer,
                    onion: "self.onion".into(),
                    mailboxes: vec!["mb1.onion".into()],
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                signature: Signature([0u8; 64]),
            })
            .unwrap();

        // Seed a due DIRECT outbox row for the peer.
        OutboxRepo::new(&pool)
            .insert_direct(&peer.0, &mid, b"ct", 0)
            .unwrap();

        // Factory that accepts one deposit on mb1.onion.
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(deposit_ok_server(server_side));
        let factory = std::sync::Arc::new(StubFactory {
            slots: StdMutex::new(vec![client_side]),
        });
        let (events_tx, _events_rx) =
            tokio::sync::broadcast::channel::<crate::daemon::events::Event>(8);
        let identity = std::sync::Arc::new(IdentityKey::generate().unwrap());
        let shared = std::sync::Arc::new(crate::delivery::hub::MailboxFallbackShared {
            factory: factory.clone(),
            events: events_tx,
            identity,
        });

        // Spawn full_run directly: no conn, no dialer → every send fails.
        let (job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let (_welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<tokio::io::DuplexStream>>(4);
        let run_pool = pool.clone();
        let handle = tokio::spawn(async move {
            let _ = super::full_run::<tokio::io::DuplexStream>(
                peer,
                None,
                job_rx,
                welcome_rx,
                ctrl_rx,
                run_pool,
                None,
                None,
                std::time::Duration::from_millis(150),
                Some(shared),
            )
            .await;
        });

        // Submit a job so the failure timer arms (no conn → ack Err, arm).
        let (ack_tx, ack_rx) = oneshot::channel::<std::result::Result<(), ()>>();
        job_tx
            .send(DeliveryJob {
                message_id: crate::envelope::MessageId(mid),
                ciphertext: vec![0xCC],
                ack_tx,
            })
            .await
            .unwrap();
        // Job must NACK (no conn).
        assert!(ack_rx.await.unwrap().is_err());

        // Poll: within ~3s the direct row must no longer be Direct.
        let mut flipped = false;
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let still_direct = OutboxRepo::new(&pool)
                .find_direct_id(&peer.0, &mid)
                .unwrap()
                .is_some();
            if !still_direct {
                flipped = true;
                break;
            }
        }
        assert!(
            flipped,
            "sustained direct failure must retarget the direct row via mailbox fallback"
        );

        handle.abort();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_task).await;
        drop(job_tx);
    }

    #[tokio::test]
    async fn retry_tick_skips_mailbox_rows() {
        use crate::storage::outbox::OutboxRepo;

        let actor_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();
        let peer = PublicKey(responder_id.public().0);

        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);

        // Handshakes run concurrently; keep the responder end to read from.
        let responder_task = tokio::spawn(async move {
            handshake_responder(server_stream, &responder_id, None)
                .await
                .unwrap()
        });
        let (actor_conn, _) =
            handshake_initiator(client_stream, &actor_id, &responder_static, None)
                .await
                .unwrap();
        let (mut responder_conn, _) = responder_task.await.unwrap();

        // Seed a DUE mailbox-kind outbox row targeting the peer.
        let pool = std::sync::Arc::new(Pool::in_memory());
        OutboxRepo::new(&pool)
            .insert_for_mailbox(&peer.0, &[0x01; 16], 7, b"ct", 0)
            .unwrap();

        let (_job_tx, job_rx) = mpsc::channel::<DeliveryJob>(4);
        let handle = PeerConnection::spawn_full_for_test(
            peer,
            Box::new(actor_conn),
            job_rx,
            pool.clone(),
            None,
        );

        // Read for longer than one retry tick (1 s). No MlsApp must arrive.
        match tokio::time::timeout(
            std::time::Duration::from_millis(1_500),
            responder_conn.recv(),
        )
        .await
        {
            Err(_) => { /* timeout: no frame arrived — correct */ }
            Ok(Ok(Some(Frame::MlsApp(_)))) => {
                panic!("mailbox-kind row must NOT be sent as direct MlsApp")
            }
            Ok(other) => panic!("unexpected frame: {other:?}"),
        }

        handle.abort();
    }
}
