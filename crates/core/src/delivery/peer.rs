// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Per-peer connection actor.

use std::collections::HashMap;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::envelope::MessageId;
use crate::error::{CoreError, Result};
use crate::identity::PublicKey;
use crate::storage::attachments::TerminalStatus;
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

/// Send `Frame::ChunkRequest` for each index over `conn`. Returns `false` if the
/// send failed (caller should drop the conn).
async fn send_chunk_requests<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    attachment_id: [u8; 16],
    indices: &[u32],
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Some(c) = conn.as_mut() else { return false };
    for &index in indices {
        if c.send(Frame::ChunkRequest {
            attachment_id,
            index,
        })
        .await
        .is_err()
        {
            return false;
        }
    }
    true
}

/// Begin the next queued inbound attachment if none is active. Sends the first
/// request window. Returns the new `active_rx` (or `None` if nothing to start /
/// send failed). The caller assigns the result to its `active_rx`.
///
/// If a dequeued begin is *already complete* — every chunk was previously
/// received and persisted (the resume / duplicate-manifest edge) — it is
/// finalized in place (emitting `Event::AttachmentReceived`) and the loop
/// advances to the next queue entry rather than installing a stuck-complete
/// `active_rx` that nothing would ever finalize.
async fn maybe_start_next_rx<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    pool: &std::sync::Arc<crate::storage::Pool>,
    inbound: &Option<std::sync::Arc<dyn InboundDispatch>>,
    peer: PublicKey,
    rx_queue: &mut std::collections::VecDeque<crate::delivery::chunk_transfer::AttachmentBegin>,
) -> Option<crate::delivery::chunk_transfer::ChunkRx>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let begin = rx_queue.pop_front()?;
        let repo = crate::storage::attachments::AttachmentRepo::new(pool);
        let already = repo
            .received_indices(&begin.attachment_id)
            .unwrap_or_default();
        let mut rx = crate::delivery::chunk_transfer::ChunkRx::new(begin.manifest, &already);
        if rx.is_complete() {
            // Already had everything (resume / duplicate-manifest edge): finalize
            // now and advance, rather than blocking the FIFO head with an rx that
            // no Chunk frame would ever complete.
            finalize_rx(conn, pool, inbound, peer, &rx).await;
            continue;
        }
        let reqs = rx.next_requests();
        let aid = rx.attachment_id();
        let (_, total) = rx.progress();
        tracing::info!(
            aid = %hex::encode(aid),
            total,
            "attachment: fetching from peer"
        );
        let _ = send_chunk_requests(conn, aid, &reqs).await;
        return Some(rx);
    }
}

/// Emit `Event::AttachmentReceived` and send `AttachmentComplete` to the
/// sender. Encrypted chunks are retained in the `ChunkStore` for on-demand
/// decryption; nothing is written to the download dir here.
async fn finalize_rx<S>(
    conn: &mut Option<AuthenticatedConnection<S>>,
    pool: &std::sync::Arc<crate::storage::Pool>,
    inbound: &Option<std::sync::Arc<dyn InboundDispatch>>,
    peer: PublicKey,
    rx: &crate::delivery::chunk_transfer::ChunkRx,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let manifest = rx.manifest();
    let aid = manifest.attachment_id;
    let safe_name = crate::delivery::chunk_transfer::sanitize_filename(&manifest.filename);
    let repo = crate::storage::attachments::AttachmentRepo::new(pool);
    // Fire-gate: only the lane that flips pending→complete emits, so the
    // direct/offline lanes can't race and produce duplicate events. On a real
    // storage failure, don't ack (so the sender retries) and leave the row pending.
    let won = match repo.claim_terminal(&aid, TerminalStatus::Complete) {
        Ok(won) => won,
        Err(e) => {
            tracing::warn!(err = %e, "inbound: attachment completion CAS failed");
            return;
        }
    };
    let (_, total) = rx.progress();
    tracing::info!(
        aid = %hex::encode(aid),
        total,
        won,
        "attachment: transfer complete"
    );
    if won {
        // Encrypted-at-rest: do NOT reassemble and do NOT remove chunks here.
        // The chunks stay in the ChunkStore; plaintext is produced only on an
        // explicit OpenAttachment/SaveAttachment. Emit availability so the UI can
        // surface Open/Save.
        if let Some(d) = inbound.as_ref() {
            d.attachment_received(peer, aid, &safe_name, &manifest.mime, manifest.total_size);
        }
    }
    // Ack on the wire regardless (we finalized, or the offline lane already did).
    if let Some(c) = conn.as_mut() {
        let _ = c
            .send(Frame::AttachmentComplete { attachment_id: aid })
            .await;
    }
}

/// Mark an inbound transfer failed and emit `Event::AttachmentFailed`.
///
/// Gated on the same terminal claim as `finalize_rx` (#38): this lane's
/// `ChunkRx` bitmap is in-memory only, so when the offline (3.C) lane fills the
/// last chunks the direct lane keeps waiting and eventually times out. Failing
/// unconditionally there would overwrite `status='complete'` and emit
/// `AttachmentFailed` for a fully-received file — which `open_attachment_cmd`
/// and `attachment_available_cmd` (both require `status == "complete"`) would
/// then refuse to hand back. Only the lane that claims the pending row reports.
async fn fail_rx(
    pool: &std::sync::Arc<crate::storage::Pool>,
    inbound: &Option<std::sync::Arc<dyn InboundDispatch>>,
    rx: &crate::delivery::chunk_transfer::ChunkRx,
    reason: &str,
) {
    let aid = rx.attachment_id();
    let repo = crate::storage::attachments::AttachmentRepo::new(pool);
    match repo.claim_terminal(&aid, TerminalStatus::Failed) {
        Ok(true) => {
            if let Some(d) = inbound.as_ref() {
                d.attachment_failed(aid, reason);
            }
        }
        Ok(false) => {
            // Another lane already terminalized this attachment (typically the
            // offline lane completing it). Nothing to report.
            tracing::debug!(
                aid = %hex::encode(aid),
                reason,
                "attachment: failure not claimed; another lane already finished it"
            );
        }
        Err(e) => {
            // Ownership unknown — stay silent rather than risk a spurious
            // failure event, and leave the row pending.
            tracing::warn!(
                aid = %hex::encode(aid), err = %e,
                "attachment: failure claim failed"
            );
        }
    }
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

    /// Try to interpret an inbound mailbox deposit as an attachment chunk
    /// (Phase 3.C offline lane): match `sha256(ciphertext)` against the chunk
    /// hashes of pending `direction='in'` manifests; on a match, store + mark
    /// received (and reassemble on completion). Returns `true` if the deposit
    /// was a (recognized) chunk and should be deleted from the mailbox server
    /// — including the dedup case where the chunk was already held. Returns
    /// `false` if it is not a chunk (caller falls through to MLS dispatch).
    /// Default `false` so non-attachment impls are unaffected.
    fn dispatch_attachment_chunk(&self, _ciphertext: &[u8]) -> bool {
        false
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
    // `AttachmentBegin` is intentionally `pub(crate)` (module-visibility
    // discipline); this trait is `pub` only because it is re-exported for the
    // `test-harness` integration tests, so the private_interfaces lint is a
    // false positive here.
    #[allow(private_interfaces)]
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
        chunk_store: Option<std::sync::Arc<crate::attachment::store::ChunkStore>>,
        download_dir: Option<std::path::PathBuf>,
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
                chunk_store,
                download_dir,
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
                None,
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
    chunk_store: Option<std::sync::Arc<crate::attachment::store::ChunkStore>>,
    download_dir: Option<std::path::PathBuf>,
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

    // 3.B inbound chunk-transfer state: at most one active inbound attachment
    // per peer; later begins queue FIFO. `None` chunk_store/download_dir
    // (test constructors) disables all chunk handling.
    let mut active_rx: Option<crate::delivery::chunk_transfer::ChunkRx> = None;
    // Sender-side served-index state, so we can report honest outbound
    // progress. Per-actor and in-memory: persists across reconnects within
    // this actor's lifetime (redials do not clear it), and is dropped only
    // on actor teardown or `forget` at completion (#114).
    let mut served = crate::delivery::chunk_transfer::ServedTracker::default();
    let mut rx_queue: std::collections::VecDeque<crate::delivery::chunk_transfer::AttachmentBegin> =
        std::collections::VecDeque::new();
    let chunk_enabled = chunk_store.is_some() && download_dir.is_some();

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
                if let Err(e) = c.send(Frame::MlsApp(job.ciphertext)).await {
                    // Surface the failure instead of discarding it via
                    // `.is_err()` (issue #75). An oversized frame (e.g. a
                    // manifest above the Noise cap) is a permanent per-message
                    // error, not a transient link failure; logging it makes it
                    // diagnosable until sender-side delivery status (v1.1) can
                    // surface it in the UI. Control flow is unchanged.
                    tracing::warn!(err = %e, "peer: send failed; dropping connection");
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
                    // Dial to the peer's onion failed — the Welcome never left
                    // this side (#90). Redaction-safe: no onion/pubkey logged.
                    tracing::warn!(
                        "peer: welcome not sent — could not establish a connection to peer"
                    );
                    let _ = wj.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                }
                let Some(c) = conn.as_mut() else {
                    tracing::warn!(
                        "peer: welcome not sent — connection slot empty after ensure_conn"
                    );
                    let _ = wj.ack_tx.send(Err(()));
                    arm_failure(&mut first_failure_at);
                    continue;
                };
                if let Err(e) = c.send(Frame::MlsWelcome(wj.welcome_bytes)).await {
                    // Connection was live but the Welcome frame write failed
                    // (dropped mid-send) — #90. `TransportErrorKind` Display is
                    // onion-/secret-free.
                    tracing::warn!(
                        err = %e,
                        "peer: welcome send failed — frame write on live connection errored"
                    );
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
                // One tick drives three jobs: (1) retry due direct-outbox rows,
                // (2) fire the sustained-failure → mailbox fallback below, and
                // (3) time out stale chunk requests. Dial-on-demand still happens
                // on the next job send; this tick never dials on its own.
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
                if chunk_enabled {
                    // Compute the timeout action (and the aid) under a short-lived
                    // mutable borrow, then release it before any `active_rx.take()`.
                    let timeout = active_rx
                        .as_mut()
                        .map(|rx| (rx.attachment_id(), rx.timed_out(std::time::Instant::now())));
                    if let Some((aid, action)) = timeout {
                        match action {
                            crate::delivery::chunk_transfer::ChunkAction::Request(idxs)
                                if !idxs.is_empty() =>
                            {
                                let _ = send_chunk_requests(&mut conn, aid, &idxs).await;
                            }
                            crate::delivery::chunk_transfer::ChunkAction::Fail => {
                                if let Some(rx) = active_rx.take() {
                                    fail_rx(&pool, &inbound, &rx, "request timeout").await;
                                }
                                active_rx = maybe_start_next_rx(
                                    &mut conn,
                                    &pool,
                                    &inbound,
                                    peer,
                                    &mut rx_queue,
                                )
                                .await;
                            }
                            _ => {}
                        }
                    }
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
                        // 3.B: resume an in-flight inbound transfer on reconnect by
                        // re-issuing its outstanding requests over the fresh conn.
                        if let Some(rx) = active_rx.as_ref() {
                            let aid = rx.attachment_id();
                            let reqs = rx.reissue();
                            let _ = send_chunk_requests(&mut conn, aid, &reqs).await;
                        }
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
                            // 3.B: a Kind::File manifest may have queued a begin.
                            if chunk_enabled {
                                while let Some(begin) = d.take_begin_attachment(peer) {
                                    rx_queue.push_back(begin);
                                }
                                if active_rx.is_none() {
                                    active_rx = maybe_start_next_rx(
                                        &mut conn,
                                        &pool,
                                        &inbound,
                                        peer,
                                        &mut rx_queue,
                                    )
                                    .await;
                                }
                            }
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
                    Ok(Some(Frame::ChunkRequest { attachment_id, index })) => {
                        last_traffic = tokio::time::Instant::now();
                        if let Some(store) = chunk_store.as_ref() {
                            let row = crate::storage::attachments::AttachmentRepo::new(&pool)
                                .get(&attachment_id)
                                .ok()
                                .flatten();
                            let total = row.as_ref().map(|r| r.total_chunks as u32).unwrap_or(0);
                            let reply = if row.as_ref().map_or(true, |r| r.direction != "out") {
                                tracing::debug!(
                                    index,
                                    "peer: nacking chunk request (unknown or not-outbound attachment)"
                                );
                                Frame::ChunkNack {
                                    attachment_id,
                                    index,
                                    reason: crate::delivery::chunk_transfer::NACK_UNKNOWN_ATTACHMENT,
                                }
                            } else {
                                if served.is_new(&attachment_id) {
                                    tracing::info!(
                                        aid = %hex::encode(attachment_id),
                                        total,
                                        "attachment: serving to peer"
                                    );
                                }
                                crate::delivery::chunk_transfer::serve_chunk_request(
                                    store,
                                    &attachment_id,
                                    index,
                                    total,
                                )
                            };
                            // Only a real Chunk reply counts as served — a nack
                            // means we could not serve this index.
                            let served_this_reply =
                                matches!(reply, Frame::Chunk { .. }).then_some(index);
                            if let Some(c) = conn.as_mut() {
                                if let Err(e) = c.send(reply).await {
                                    tracing::warn!(
                                        err = %e,
                                        "peer: failed to send chunk reply; dropping connection"
                                    );
                                    conn = None;
                                    drain_pending(&mut pending);
                                } else if let Some(i) = served_this_reply {
                                    let count = served.record(&attachment_id, i);
                                    if let Some(d) = inbound.as_ref() {
                                        d.attachment_progress(attachment_id, count, total);
                                    }
                                }
                            }
                        } else {
                            // A peer asked us to serve a chunk but this actor has
                            // no ChunkStore, so the request can only time out on
                            // the requester's side. Log it so this failure mode
                            // (issue #76) is diagnosable rather than silent.
                            tracing::warn!(
                                index,
                                "peer: ChunkRequest received but no ChunkStore configured; cannot serve"
                            );
                        }
                    }
                    Ok(Some(Frame::Chunk { attachment_id, index, ciphertext })) => {
                        last_traffic = tokio::time::Instant::now();
                        if chunk_enabled {
                            let matches = active_rx.as_ref().map(|r| r.attachment_id())
                                == Some(attachment_id);
                            // Borrow split: take rx out only when it matches (so a
                            // non-matching active transfer is never dropped), operate,
                            // put back.
                            if let Some(mut rx) = if matches { active_rx.take() } else { None } {
                                if rx.verify(index, &ciphertext) {
                                    if let Some(store) = chunk_store.as_ref() {
                                        let _ = store.put(&attachment_id, index, &ciphertext);
                                    }
                                    let repo =
                                        crate::storage::attachments::AttachmentRepo::new(&pool);
                                    let _ = repo.mark_received(&attachment_id, index);
                                    rx.on_received(index);
                                    let (recv, total) = rx.progress();
                                    tracing::debug!(
                                        aid = %hex::encode(attachment_id),
                                        index,
                                        received = recv,
                                        total,
                                        "attachment: chunk received"
                                    );
                                    if let Some(d) = inbound.as_ref() {
                                        // Throttle: every 8th chunk or on completion.
                                        if recv % 8 == 0 || recv == total {
                                            d.attachment_progress(attachment_id, recv, total);
                                        }
                                    }
                                    if rx.is_complete() {
                                        finalize_rx(
                                            &mut conn,
                                            &pool,
                                            &inbound,
                                            peer,
                                            &rx,
                                        )
                                        .await;
                                        active_rx = maybe_start_next_rx(
                                            &mut conn,
                                            &pool,
                                            &inbound,
                                            peer,
                                            &mut rx_queue,
                                        )
                                        .await;
                                    } else {
                                        let reqs = rx.next_requests();
                                        let aid = rx.attachment_id();
                                        let _ =
                                            send_chunk_requests(&mut conn, aid, &reqs).await;
                                        active_rx = Some(rx);
                                    }
                                } else {
                                    // Hash mismatch → retry or fail.
                                    match rx.on_bad(index) {
                                        crate::delivery::chunk_transfer::ChunkAction::Request(
                                            idxs,
                                        ) => {
                                            let aid = rx.attachment_id();
                                            let _ = send_chunk_requests(&mut conn, aid, &idxs)
                                                .await;
                                            active_rx = Some(rx);
                                        }
                                        crate::delivery::chunk_transfer::ChunkAction::Fail => {
                                            fail_rx(&pool, &inbound, &rx, "chunk hash mismatch")
                                                .await;
                                            active_rx = maybe_start_next_rx(
                                                &mut conn,
                                                &pool,
                                                &inbound,
                                                peer,
                                                &mut rx_queue,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Some(Frame::ChunkNack { attachment_id, index: _, reason })) => {
                        last_traffic = tokio::time::Instant::now();
                        let nack_matches = chunk_enabled
                            && active_rx.as_ref().map(|r| r.attachment_id())
                                == Some(attachment_id);
                        if let Some(rx) = if nack_matches { active_rx.take() } else { None } {
                            fail_rx(
                                &pool,
                                &inbound,
                                &rx,
                                &format!("sender nack reason {reason}"),
                            )
                            .await;
                            active_rx = maybe_start_next_rx(
                                &mut conn,
                                &pool,
                                &inbound,
                                peer,
                                &mut rx_queue,
                            )
                            .await;
                        }
                    }
                    Ok(Some(Frame::AttachmentComplete { attachment_id })) => {
                        last_traffic = tokio::time::Instant::now();
                        tracing::info!(
                            aid = %hex::encode(attachment_id),
                            "attachment: delivery acked by peer"
                        );
                        // Sender side: receiver confirmed receipt → finalize the out row + GC.
                        // These three writes are best-effort — the peer already has
                        // the file, so nothing is retried on failure — but a silent
                        // failure leaves the row stuck non-`complete`, the staged
                        // chunks on disk, or the offline deposits queued to re-send
                        // an attachment that already arrived. Log each one.
                        let repo = crate::storage::attachments::AttachmentRepo::new(&pool);
                        if let Err(e) = repo.set_status(&attachment_id, "complete") {
                            tracing::warn!(
                                aid = %hex::encode(attachment_id), err = %e,
                                "attachment: set_status(complete) failed after peer ack"
                            );
                        }
                        if let Some(store) = chunk_store.as_ref() {
                            if let Err(e) = store.remove(&attachment_id) {
                                tracing::warn!(
                                    aid = %hex::encode(attachment_id), err = %e,
                                    "attachment: staged chunk GC failed after peer ack"
                                );
                            }
                        }
                        served.forget(&attachment_id);
                        // 3.C: a direct completion cancels the offline lane.
                        if let Err(e) = crate::storage::attachments::AttachmentDepositRepo::new(
                            &pool,
                        )
                        .delete_for_attachment(&attachment_id)
                        {
                            tracing::warn!(
                                aid = %hex::encode(attachment_id), err = %e,
                                "attachment: offline deposit prune failed after peer ack; \
                                 chunks may be re-deposited to a mailbox"
                            );
                        }
                        if let Some(d) = inbound.as_ref() {
                            if let Ok(Some(row)) = repo.get(&attachment_id) {
                                let t = row.total_chunks as u32;
                                d.attachment_progress(attachment_id, t, t);
                            }
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
                None,
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
                None,
                None,
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

    /// Actor-level in-session resume (Phase 3.B). Drives `full_run` directly as
    /// the RECEIVER with real Noise conns. Proves the core resume invariant:
    /// after a `PeerCtrl::ReplaceConn` mid-transfer, the actor re-issues its
    /// outstanding chunk requests over the FRESH conn (`rx.reissue()`), then
    /// completes the transfer and reassembles the byte-identical plaintext.
    ///
    /// The full-stack guardrail (a real dropped conn mid-transfer) is infeasible
    /// over loopback — no public action deterministically severs a connection
    /// mid-stream — so we inject the reconnect via `ReplaceConn` directly.
    #[tokio::test]
    async fn resume_reissues_chunk_requests_and_completes_after_replace_conn() {
        use crate::attachment::store::ChunkStore;
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;
        use tokio::io::DuplexStream;

        // --- Build a real 3-chunk attachment we will play the SENDER for. ---
        let payload = vec![0x5Au8; crate::attachment::CHUNK_SIZE * 2 + 100];
        let (manifest, ciphertexts) = crate::attachment::chunker::chunk_plaintext(
            &payload,
            "f.bin",
            "application/octet-stream",
        )
        .unwrap();
        assert_eq!(manifest.chunks.len(), 3, "expected a 3-chunk attachment");
        let aid = manifest.attachment_id;

        // --- Stub InboundDispatch: yields exactly one AttachmentBegin and
        // records completion / failure. ---
        type Received = Arc<StdMutex<Option<[u8; 16]>>>;
        struct Stub {
            begin: StdMutex<Option<crate::delivery::chunk_transfer::AttachmentBegin>>,
            received: Received,
            failed: Arc<StdMutex<Option<String>>>,
        }
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                // Represents the manifest MlsApp "decrypting" → actor ACKs it.
                Some(MessageId::generate())
            }
            #[allow(private_interfaces)]
            fn take_begin_attachment(
                &self,
                _peer: PublicKey,
            ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
                self.begin.lock().unwrap().take()
            }
            fn attachment_received(
                &self,
                _peer: PublicKey,
                aid: [u8; 16],
                _filename: &str,
                _mime: &str,
                _size: u64,
            ) {
                *self.received.lock().unwrap() = Some(aid);
            }
            fn attachment_failed(&self, _aid: [u8; 16], reason: &str) {
                *self.failed.lock().unwrap() = Some(reason.to_string());
            }
        }

        let received: Received = Arc::new(StdMutex::new(None));
        let failed: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let stub = Arc::new(Stub {
            begin: StdMutex::new(Some(crate::delivery::chunk_transfer::AttachmentBegin {
                attachment_id: aid,
                manifest: manifest.clone(),
            })),
            received: received.clone(),
            failed: failed.clone(),
        });

        // --- Receiver-side state: pool, chunk store, download dir. ---
        let pool = std::sync::Arc::new(Pool::in_memory());
        // Mirror production: the `Kind::File` manifest dispatch (inbound.rs)
        // ALWAYS inserts a pending `direction='in'` row before the chunk fetch.
        // `finalize_rx`'s CAS fire-gate (Phase 4.D) emits AttachmentReceived only
        // when it flips that pending row → complete, so the row must exist. The
        // mock InboundDispatch injects the begin directly and bypasses that
        // insert, so we perform it here as production would.
        crate::storage::attachments::AttachmentRepo::new(&pool)
            .insert(
                &aid,
                "in",
                &manifest.to_cbor().unwrap(),
                manifest.chunks.len() as i64,
                0,
            )
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // Use tmp.path() as the data dir (chunk store root: tmp/attachments/<hex>/).
        // Use a distinct subdirectory as the download dir so the "no plaintext written"
        // assertion is not confused by the chunk store's own `attachments/` subdir.
        let chunk_store = Arc::new(ChunkStore::new(tmp.path()));
        let download_dir = tmp.path().join("downloads");

        // --- Noise handshake #1: conn1 (actor initiator) + peer_conn1 (test). ---
        let actor_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();
        // The actor (receiver) is the Noise initiator; the peer (test) is the
        // responder. `peer` is the responder's identity PublicKey.
        let peer = PublicKey(responder_id.public().0);

        async fn dial(
            actor_id: &IdentityKey,
            responder_id: IdentityKey,
            responder_static: [u8; 32],
        ) -> (
            AuthenticatedConnection<DuplexStream>,
            AuthenticatedConnection<DuplexStream>,
        ) {
            let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
            let responder_task = tokio::spawn(async move {
                handshake_responder(server_stream, &responder_id, None)
                    .await
                    .unwrap()
            });
            let (actor_conn, _) =
                handshake_initiator(client_stream, actor_id, &responder_static, None)
                    .await
                    .unwrap();
            let (peer_conn, _) = responder_task.await.unwrap();
            (actor_conn, peer_conn)
        }

        let (conn1, mut peer_conn1) = dial(&actor_id, responder_id, responder_static).await;

        // --- Spawn the receiver actor driving full_run directly. ---
        let (_jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(4);
        let (_welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<DuplexStream>>(4);
        let run_pool = pool.clone();
        let run_stub: std::sync::Arc<dyn InboundDispatch> = stub.clone();
        let run_store = chunk_store.clone();
        let run_dir = download_dir.clone();
        let actor = tokio::spawn(async move {
            let _ = super::full_run::<DuplexStream>(
                peer,
                Some(conn1),
                jobs_rx,
                welcome_rx,
                ctrl_rx,
                run_pool,
                Some(run_stub),
                None,
                Duration::ZERO,
                None,
                Some(run_store),
                Some(run_dir),
            )
            .await;
        });

        // --- Kick off the transfer: send the "manifest" as an MlsApp frame. ---
        peer_conn1
            .send(Frame::MlsApp(b"manifest".to_vec()))
            .await
            .unwrap();

        // Collect frames from conn1: the actor ACKs the manifest, then sends a
        // ChunkRequest window for the 3 missing indices (window 8 > 3).
        async fn collect_requests(
            conn: &mut AuthenticatedConnection<DuplexStream>,
            want: usize,
        ) -> Vec<u32> {
            let mut got = Vec::new();
            while got.len() < want {
                let frame = tokio::time::timeout(Duration::from_secs(3), conn.recv())
                    .await
                    .expect("recv must not time out")
                    .unwrap()
                    .expect("conn must not EOF");
                match frame {
                    Frame::Ack(_) => { /* manifest ACK — drain */ }
                    Frame::ChunkRequest { index, .. } => got.push(index),
                    other => panic!("unexpected frame while collecting requests: {other:?}"),
                }
            }
            got
        }

        let mut reqs1 = collect_requests(&mut peer_conn1, 3).await;
        reqs1.sort_unstable();
        assert_eq!(
            reqs1,
            vec![0, 1, 2],
            "first window must request all 3 indices"
        );

        // --- Simulate reconnect: hand the actor a FRESH conn via ReplaceConn. ---
        // Re-create handshake material for conn2. (responder_id was moved into
        // dial #1, so generate a fresh peer identity for the reconnect; `peer`
        // stays the same — the actor only uses the PublicKey it was spawned
        // with for dispatch, not the conn's handshake identity.)
        let responder_id2 = IdentityKey::generate().unwrap();
        let responder_static2 = responder_id2.noise_static_public();
        let (conn2, mut peer_conn2) = dial(&actor_id, responder_id2, responder_static2).await;

        ctrl_tx
            .send(PeerCtrl::ReplaceConn(Box::new(conn2)))
            .await
            .unwrap();

        // --- CORE RESUME ASSERTION: the SAME outstanding indices must be
        // re-issued as ChunkRequests over the FRESH conn2. ---
        let mut reqs2 = collect_requests(&mut peer_conn2, 3).await;
        reqs2.sort_unstable();
        assert_eq!(
            reqs2,
            vec![0, 1, 2],
            "after ReplaceConn the actor must re-issue ALL outstanding chunk \
             requests over the fresh conn (this is the resume proof)"
        );

        // --- Complete the transfer over conn2: serve each requested chunk. The
        // actor verifies + stores each, refilling the window as needed. Since
        // all 3 were requested up front, serving all 3 completes it. ---
        for &index in &reqs2 {
            peer_conn2
                .send(Frame::Chunk {
                    attachment_id: aid,
                    index,
                    ciphertext: ciphertexts[index as usize].clone(),
                })
                .await
                .unwrap();
        }

        // The actor reassembles on completion and sends AttachmentComplete. Drain
        // any trailing frames (AttachmentComplete / further ChunkRequests) so the
        // conn stays alive while we poll for completion.
        let drain = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(Duration::from_millis(200), peer_conn2.recv()).await {
                    Ok(Ok(Some(Frame::Chunk {
                        attachment_id,
                        index,
                        ciphertext,
                    }))) => {
                        // (Shouldn't happen — receiver doesn't request from us
                        // again — but keep the loop honest.)
                        let _ = (attachment_id, index, ciphertext);
                    }
                    Ok(Ok(Some(_))) => {}
                    _ => break,
                }
            }
        });

        // --- Assert completion within a timeout. ---
        let mut done: Option<[u8; 16]> = None;
        for _ in 0..50 {
            if let Some(rec) = *received.lock().unwrap() {
                done = Some(rec);
                break;
            }
            assert!(
                failed.lock().unwrap().is_none(),
                "transfer must not fail: {:?}",
                failed.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let got_aid = done.expect("attachment_received must fire within 5s");
        assert_eq!(
            got_aid, aid,
            "completed attachment id must match the manifest"
        );
        assert!(
            failed.lock().unwrap().is_none(),
            "attachment_failed must never fire"
        );

        // --- Encrypted-at-rest: completion must NOT write plaintext to the download
        // dir; encrypted chunks must be retained for on-demand decrypt. ---
        // Create the download dir up front and `expect` the read so a missing /
        // unreadable dir FAILS the test rather than passing vacuously.
        std::fs::create_dir_all(&download_dir).expect("create download dir");
        let entries: Vec<_> = std::fs::read_dir(&download_dir)
            .expect("download dir must be readable")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        assert!(
            entries.is_empty(),
            "completion must leave no plaintext in the download dir, found: {entries:?}"
        );
        let chunk_dir = tmp.path().join("attachments").join(hex::encode(aid));
        assert!(
            chunk_dir.is_dir() && std::fs::read_dir(&chunk_dir).unwrap().count() > 0,
            "encrypted chunks must be retained after completion"
        );

        drop(ctrl_tx);
        actor.abort();
        drain.abort();
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

    /// Build a real single-chunk attachment and return `(manifest, complete rx)`.
    fn one_chunk_rx() -> (
        crate::attachment::manifest::AttachmentManifest,
        crate::delivery::chunk_transfer::ChunkRx,
    ) {
        let (manifest, _cts) = crate::attachment::chunker::chunk_plaintext(
            b"hello",
            "f.bin",
            "application/octet-stream",
        )
        .unwrap();
        assert_eq!(manifest.chunks.len(), 1, "expected a 1-chunk attachment");
        let rx = crate::delivery::chunk_transfer::ChunkRx::new(manifest.clone(), &[0]);
        assert!(rx.is_complete(), "rx must be complete for a finalize test");
        (manifest, rx)
    }

    /// #38 — a late direct-lane failure must NOT clobber an attachment the
    /// offline (3.C) lane already completed. Both lanes race whenever the
    /// sender goes offline mid-transfer: the mailbox lane fills the last chunks
    /// and wins the terminal claim, then the direct lane's in-memory `ChunkRx`
    /// (which never learns the gaps were filled) hits its 30 s request timeout.
    /// Before the terminal-claim gate, that timeout overwrote `status='complete'`
    /// with `'failed'` and emitted `AttachmentFailed`, leaving a fully-received
    /// file permanently unopenable — `open_attachment_cmd` and
    /// `attachment_available_cmd` both require `status == "complete"`.
    #[tokio::test]
    async fn fail_rx_does_not_clobber_an_already_completed_attachment() {
        use crate::storage::attachments::{AttachmentRepo, TerminalStatus};
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;

        struct Stub(Arc<StdMutex<Option<String>>>);
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
            fn attachment_failed(&self, _aid: [u8; 16], reason: &str) {
                *self.0.lock().unwrap() = Some(reason.to_string());
            }
        }

        let (manifest, rx) = one_chunk_rx();
        let aid = manifest.attachment_id;
        let pool = Arc::new(Pool::in_memory());
        let repo = AttachmentRepo::new(&pool);
        repo.insert(&aid, "in", &manifest.to_cbor().unwrap(), 1, 0)
            .unwrap();
        // The offline lane got there first.
        assert!(repo.claim_terminal(&aid, TerminalStatus::Complete).unwrap());

        let failed: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let inbound: Option<Arc<dyn InboundDispatch>> = Some(Arc::new(Stub(failed.clone())));
        fail_rx(&pool, &inbound, &rx, "request timeout").await;

        assert_eq!(
            repo.get(&aid).unwrap().unwrap().status,
            "complete",
            "a late failure must not overwrite the completed status"
        );
        assert!(
            failed.lock().unwrap().is_none(),
            "a late failure must not emit AttachmentFailed for a completed attachment"
        );
    }

    /// #38 acceptance — simultaneous direct + offline completion emits exactly
    /// one `Event::AttachmentReceived`, in either arrival order. Both lanes are
    /// driven for real (`finalize_rx` and `DaemonInbound::finalize_offline`)
    /// against one shared pool and one event channel; the terminal claim is the
    /// only thing stopping a double emit.
    #[tokio::test]
    async fn direct_and_offline_completion_emit_one_attachment_received() {
        use crate::daemon::events::Event;
        use crate::daemon::inbound::DaemonInbound;
        use crate::storage::attachments::AttachmentRepo;
        use std::sync::Arc;
        use tokio::io::DuplexStream;
        use tokio::sync::broadcast;

        // `direct_first == true` runs finalize_rx then finalize_offline.
        async fn run_both_lanes(direct_first: bool) -> Vec<Event> {
            let (manifest, rx) = one_chunk_rx();
            let aid = manifest.attachment_id;
            let pool = Arc::new(Pool::in_memory());
            AttachmentRepo::new(&pool)
                .insert(&aid, "in", &manifest.to_cbor().unwrap(), 1, 0)
                .unwrap();

            let (events_tx, mut events_rx) = broadcast::channel(16);
            let dispatch = Arc::new(DaemonInbound::new(pool.clone(), events_tx));
            let inbound: Option<Arc<dyn InboundDispatch>> = Some(dispatch.clone());
            let peer = PublicKey([0x07; 32]);
            // No conn: the AttachmentComplete ack is not what is under test.
            let mut conn: Option<AuthenticatedConnection<DuplexStream>> = None;

            if direct_first {
                finalize_rx(&mut conn, &pool, &inbound, peer, &rx).await;
                dispatch.finalize_offline(&aid, &manifest);
            } else {
                dispatch.finalize_offline(&aid, &manifest);
                finalize_rx(&mut conn, &pool, &inbound, peer, &rx).await;
            }

            let mut seen = Vec::new();
            while let Ok(ev) = events_rx.try_recv() {
                seen.push(ev);
            }
            seen
        }

        for direct_first in [true, false] {
            let received: Vec<_> = run_both_lanes(direct_first)
                .await
                .into_iter()
                .filter(|e| matches!(e, Event::AttachmentReceived { .. }))
                .collect();
            assert_eq!(
                received.len(),
                1,
                "exactly one AttachmentReceived expected (direct_first={direct_first}), got {received:?}"
            );
        }
    }
}
