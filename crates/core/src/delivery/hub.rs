// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Daemon-scoped delivery router.
//!
//! Maps `PublicKey → (mpsc::Sender<DeliveryJob>, mpsc::Sender<PeerCtrl>)`,
//! spawning a per-peer actor on the first send or ingest. Also runs a
//! periodic `seen_messages` sweep, and (Task 20) a direct → mailbox
//! fallback orchestrator that retargets an outbox row to one of the
//! recipient's advertised mailboxes when direct delivery fails.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

use crate::daemon::events::{DeliveryStatus, Event};
use crate::delivery::peer::{DeliveryJob, InboundDispatch, PeerConnection, PeerCtrl};
use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::{IdentityKey, PublicKey};
use crate::mailbox::client::recipient_hash_from_pubkey;
use crate::mailbox::poll::MailboxConnectFactory;
use crate::storage::outbox::OutboxRepo;
use crate::storage::seen_messages::SeenMessagesRepo;
use crate::storage::{MailboxRepo, Pool};
use crate::transport::connection::AuthenticatedConnection;

const JOB_CHAN_CAP: usize = 64;
const CTRL_CHAN_CAP: usize = 4;
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const SEEN_WINDOW_MS: i64 = 24 * 3600 * 1000;

/// Default TTL request for mailbox deposits made via fallback. 24 hours
/// is long enough to cover most "peer is briefly offline" windows
/// without unnecessarily encouraging the server to retain ciphertext.
const FALLBACK_TTL_SECS: u32 = 24 * 3600;

/// Default per-peer direct→mailbox fallback timeout used by constructors
/// that don't take one from config (matches `default_direct_timeout_secs`).
const DEFAULT_DIRECT_TIMEOUT: Duration = std::time::Duration::from_secs(30);

struct PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    jobs: mpsc::Sender<DeliveryJob>,
    welcome_jobs: mpsc::Sender<crate::delivery::peer::WelcomeJob>,
    ctrl: mpsc::Sender<PeerCtrl<S>>,
}

impl<S> Clone for PeerChannels<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            jobs: self.jobs.clone(),
            welcome_jobs: self.welcome_jobs.clone(),
            ctrl: self.ctrl.clone(),
        }
    }
}

/// Mailbox-fallback dependencies, kept together and shareable via `Arc` so the
/// hub, the per-peer actor, and the sweeper can all run deposits without a
/// reference to the generic `DeliveryHub<S>` (avoids an Arc cycle — the hub
/// owns the per-peer actors).
pub(crate) struct MailboxFallbackShared {
    pub(crate) factory: Arc<dyn MailboxConnectFactory>,
    pub(crate) events: broadcast::Sender<Event>,
    /// Held for forward-compat with signed-deposit variants; the current
    /// Deposit frame is depositor-anonymous so identity isn't used at deposit
    /// time.
    #[allow(dead_code)]
    pub(crate) identity: Arc<IdentityKey>,
}

/// Daemon-scoped delivery router. Routes outbound sends and inbound
/// post-handshake connections to per-peer `PeerConnection` actor tasks.
///
/// When constructed with [`DeliveryHub::new_with_mailbox_fallback`] the
/// hub also owns a fallback orchestrator: callers may invoke
/// [`DeliveryHub::ensure_mailbox_fallback`] when direct delivery to a
/// peer has failed, and the hub will pick one of the recipient's
/// advertised mailboxes (deterministically by message id) and walk the
/// list, depositing into the first reachable one.
pub struct DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    peers: Mutex<HashMap<PublicKey, PeerChannels<S>>>,
    pool: Arc<Pool>,
    inbound: Option<Arc<dyn InboundDispatch>>,
    sweep: tokio::task::JoinHandle<()>,
    /// `None` when the hub was constructed without fallback support — in
    /// which case `ensure_mailbox_fallback` is a logged no-op.
    fallback: Option<Arc<MailboxFallbackShared>>,
    /// On-demand outbound dialer, injected so the per-peer actor can
    /// resolve + dial a peer when it has no live connection. `None` for
    /// outbound-only tests that pre-seed connections via `ingest`.
    dialer: Option<Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
    /// How long a per-peer actor tolerates UNBROKEN direct-delivery
    /// failure before handing the peer's pending direct rows to the
    /// mailbox fallback. Passed to each spawned actor.
    direct_timeout: Duration,
    /// Staged-chunk store for attachment transfer (3.B). `None` disables.
    chunk_store: Option<Arc<crate::attachment::store::ChunkStore>>,
    /// Where reassembled inbound attachments are written. `None` disables.
    download_dir: Option<std::path::PathBuf>,
}

impl<S> DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Construct a hub with no inbound-MLS handling and no mailbox
    /// fallback. Suitable for outbound-only tests where the responder
    /// echoes `Frame::Ack` directly; real-MLS deployments must use
    /// [`DeliveryHub::new_with_inbound`] or
    /// [`DeliveryHub::new_with_mailbox_fallback`] instead.
    pub fn new(pool: Arc<Pool>) -> Self {
        Self::new_inner(pool, None, None, None, DEFAULT_DIRECT_TIMEOUT, None, None)
    }

    /// Construct a hub that decrypts inbound `Frame::MlsApp` through
    /// `dispatch`. The integration test builds an `MlsInboundDispatch`
    /// that wraps `Group::decrypt` + `receiver::receive` for the one
    /// peer it cares about.
    ///
    /// No mailbox-fallback orchestrator: callers wanting fallback must
    /// use [`DeliveryHub::new_with_mailbox_fallback`].
    pub fn new_with_inbound(pool: Arc<Pool>, dispatch: Arc<dyn InboundDispatch>) -> Self {
        Self::new_inner(
            pool,
            Some(dispatch),
            None,
            None,
            DEFAULT_DIRECT_TIMEOUT,
            None,
            None,
        )
    }

    /// Construct a hub that decrypts inbound `Frame::MlsApp` AND owns an
    /// on-demand outbound `dialer`, so the per-peer actor can dial a peer
    /// when it has no live connection instead of dropping the job. This
    /// is the production direct-transport wiring (no mailbox fallback).
    pub(crate) fn new_with_inbound_and_dialer(
        pool: Arc<Pool>,
        dispatch: Arc<dyn InboundDispatch>,
        dialer: Arc<dyn crate::delivery::dial::OutboundDial<S>>,
    ) -> Self {
        Self::new_inner(
            pool,
            Some(dispatch),
            None,
            Some(dialer),
            DEFAULT_DIRECT_TIMEOUT,
            None,
            None,
        )
    }

    /// Production constructor: on-demand `dialer` AND the direct→mailbox
    /// fallback orchestrator. Used by `run_with_transport`.
    pub(crate) fn new_with_inbound_dialer_and_fallback(
        pool: Arc<Pool>,
        dispatch: Arc<dyn InboundDispatch>,
        dialer: Arc<dyn crate::delivery::dial::OutboundDial<S>>,
        fallback: Arc<MailboxFallbackShared>,
        direct_timeout: Duration,
        data_dir: &std::path::Path,
        download_dir: std::path::PathBuf,
    ) -> Self {
        let chunk_store = Some(Arc::new(crate::attachment::store::ChunkStore::new(
            data_dir,
        )));
        Self::new_inner(
            pool,
            Some(dispatch),
            Some(fallback),
            Some(dialer),
            direct_timeout,
            chunk_store,
            Some(download_dir),
        )
    }

    /// Test-only constructor: an on-demand `dialer` with no inbound-MLS
    /// handling and no mailbox fallback. Used by the dial-on-demand unit
    /// test where the responder reads the dialed frame directly.
    #[cfg(test)]
    pub(crate) fn new_with_dialer(
        pool: Arc<Pool>,
        dialer: Arc<dyn crate::delivery::dial::OutboundDial<S>>,
    ) -> Self {
        Self::new_inner(
            pool,
            None,
            None,
            Some(dialer),
            DEFAULT_DIRECT_TIMEOUT,
            None,
            None,
        )
    }

    /// Construct a hub that owns the direct → mailbox fallback
    /// orchestrator in addition to the pre-existing direct delivery
    /// path. `inbound` is `Some` in production (real MLS decrypt) and
    /// `None` in tests that don't exercise inbound decryption.
    ///
    /// `pub(crate)` because `MailboxConnectFactory` is itself
    /// `pub(crate)` — production wiring lives in `daemon::run`.
    pub(crate) fn new_with_mailbox_fallback(
        pool: Arc<Pool>,
        inbound: Option<Arc<dyn InboundDispatch>>,
        events: broadcast::Sender<Event>,
        mailbox_factory: Arc<dyn MailboxConnectFactory>,
        identity: Arc<IdentityKey>,
    ) -> Self {
        Self::new_inner(
            pool,
            inbound,
            Some(Arc::new(MailboxFallbackShared {
                factory: mailbox_factory,
                events,
                identity,
            })),
            None,
            DEFAULT_DIRECT_TIMEOUT,
            None,
            None,
        )
    }

    fn new_inner(
        pool: Arc<Pool>,
        inbound: Option<Arc<dyn InboundDispatch>>,
        fallback: Option<Arc<MailboxFallbackShared>>,
        dialer: Option<Arc<dyn crate::delivery::dial::OutboundDial<S>>>,
        direct_timeout: Duration,
        chunk_store: Option<Arc<crate::attachment::store::ChunkStore>>,
        download_dir: Option<std::path::PathBuf>,
    ) -> Self {
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
            sweep,
            fallback,
            dialer,
            direct_timeout,
            chunk_store,
            download_dir,
        }
    }

    /// Spawn a new peer actor and insert it into the map, returning the
    /// newly created channels. Callers must already hold the `peers` lock
    /// guard.
    fn spawn_peer_actor(
        &self,
        peers: &mut HashMap<PublicKey, PeerChannels<S>>,
        peer: PublicKey,
    ) -> PeerChannels<S> {
        let (jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(JOB_CHAN_CAP);
        let (welcome_jobs_tx, welcome_jobs_rx) =
            mpsc::channel::<crate::delivery::peer::WelcomeJob>(JOB_CHAN_CAP);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<S>>(CTRL_CHAN_CAP);
        let _handle = PeerConnection::spawn::<S>(
            peer,
            jobs_rx,
            welcome_jobs_rx,
            ctrl_rx,
            self.pool.clone(),
            self.inbound.clone(),
            self.dialer.clone(),
            self.direct_timeout,
            self.fallback_shared(),
            self.chunk_store.clone(),
            self.download_dir.clone(),
        );
        let channels = PeerChannels {
            jobs: jobs_tx,
            welcome_jobs: welcome_jobs_tx,
            ctrl: ctrl_tx,
        };
        peers.insert(peer, channels.clone());
        channels
    }

    /// Submit a job for `peer`. Spawns the peer actor on first use.
    pub async fn send(
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
    /// The `peers` lock is released before awaiting the ctrl send so a
    /// full channel cannot block a lock-holding await.
    pub async fn ingest(&self, peer: PublicKey, conn: AuthenticatedConnection<S>) {
        let ctrl_tx = {
            let mut peers = self.peers.lock().await;
            match peers.get(&peer) {
                Some(ch) => ch.ctrl.clone(),
                None => {
                    let channels = self.spawn_peer_actor(&mut peers, peer);
                    channels.ctrl
                }
            }
        }; // lock released here

        let _ = ctrl_tx.send(PeerCtrl::ReplaceConn(Box::new(conn))).await;
    }

    /// Dial `peer` by a caller-supplied `onion` (e.g. from an invite card,
    /// before the inviter's `ContactCard` is persisted), ingest the resulting
    /// connection, and return its `h_transport` for the caller to bind into the
    /// genesis MLS Commit (ADR 0009). Used by first-contact `add_contact`, where
    /// the card is written inside the same transaction as the genesis group,
    /// *after* this dial succeeds — full add_contact atomicity (T2-1). Errors if
    /// no dialer is wired or the dial fails.
    ///
    /// The `dialer` `Arc` is cloned because `dial_at` borrows `&self` across the
    /// `.await`; cloning avoids holding a borrow of `self.dialer`.
    pub(crate) async fn connect_and_ingest_at(
        &self,
        peer: PublicKey,
        onion: &str,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>> {
        let dialer = self.dialer.clone().ok_or_else(|| {
            crate::error::CoreError::Delivery(crate::delivery::DeliveryErrorKind::Other(
                "no dialer wired".into(),
            ))
        })?;
        let (conn, h_transport) = dialer.dial_at(peer, onion).await?;
        self.ingest(peer, conn).await;
        Ok(h_transport)
    }

    /// Whether a per-peer actor currently exists for `peer`. Used by the
    /// accept-loop test to assert an unknown peer was NOT ingested.
    pub(crate) async fn has_peer(&self, peer: &PublicKey) -> bool {
        self.peers.lock().await.contains_key(peer)
    }

    async fn ensure_actor(&self, peer: PublicKey) -> mpsc::Sender<DeliveryJob> {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            return ch.jobs.clone();
        }
        let channels = self.spawn_peer_actor(&mut peers, peer);
        channels.jobs
    }

    /// Submit a Welcome job for `peer`. Spawns the peer actor on first
    /// use. The Welcome is sent over the existing Noise_XK transport
    /// as `Frame::MlsWelcome(bytes)`. ACK correlation uses
    /// `welcome_msg_id(bytes)` (BLAKE2s prefix), which the receiver
    /// computes identically.
    ///
    /// On success: the returned oneshot resolves `Ok(())` when the
    /// peer ACKs (synchronous in the typical "Alice is online" path).
    /// On failure (no live conn, dropped actor): `Err(())`.
    pub async fn send_welcome(
        &self,
        peer: PublicKey,
        welcome_bytes: Vec<u8>,
    ) -> Result<oneshot::Receiver<std::result::Result<(), ()>>> {
        let (ack_tx, ack_rx) = oneshot::channel::<std::result::Result<(), ()>>();
        let welcome_jobs_tx = self.ensure_welcome_actor(peer).await;
        let _ = welcome_jobs_tx
            .send(crate::delivery::peer::WelcomeJob {
                welcome_bytes,
                ack_tx,
            })
            .await;
        Ok(ack_rx)
    }

    async fn ensure_welcome_actor(
        &self,
        peer: PublicKey,
    ) -> mpsc::Sender<crate::delivery::peer::WelcomeJob> {
        let mut peers = self.peers.lock().await;
        if let Some(ch) = peers.get(&peer) {
            return ch.welcome_jobs.clone();
        }
        let channels = self.spawn_peer_actor(&mut peers, peer);
        channels.welcome_jobs
    }

    /// Direct → mailbox fallback orchestrator (Task 20).
    ///
    /// When direct delivery to `peer` has failed (timeout or hard
    /// connect error), the daemon invokes this method with the
    /// outbound message's id and ciphertext. The orchestrator:
    ///
    /// 1. Looks up the peer's `ContactCard.body.mailboxes` via
    ///    [`MailboxRepo::list_for_contact`].
    /// 2. Picks a primary mailbox deterministically:
    ///    `mailboxes[blake2s(message_id) % len]`.
    /// 3. Walks `(0..len).cycle().skip(primary).take(len)`, attempting
    ///    one [`MailboxClient::deposit`] per onion. The first success
    ///    deletes the outbox row and emits
    ///    `Event::DeliveryStatusChanged{Deposited}`.
    /// 4. If every mailbox fails, leaves the outbox row in place and
    ///    logs the cascade (no event). The pre-existing outbox retry
    ///    path will surface a `Failed` event after the backoff cap.
    ///
    /// On a hub constructed without `mailbox_factory` (i.e. via
    /// [`DeliveryHub::new`] or [`DeliveryHub::new_with_inbound`]) this
    /// method logs and returns silently.
    pub async fn ensure_mailbox_fallback(
        &self,
        peer: PublicKey,
        message_id: MessageId,
        ciphertext: Vec<u8>,
    ) {
        let Some(shared) = self.fallback.as_ref() else {
            tracing::debug!(
                target: "skattr::delivery::hub",
                "fallback skipped: hub has no mailbox factory"
            );
            return;
        };
        run_mailbox_fallback(&self.pool, shared, peer, message_id, ciphertext).await;
    }

    /// Clone the shareable fallback bundle, if this hub has one. Used by the
    /// per-peer actor (timeout trigger) and the sweeper.
    pub(crate) fn fallback_shared(&self) -> Option<Arc<MailboxFallbackShared>> {
        self.fallback.clone()
    }
}

/// Retarget the existing direct outbox row for `(peer, message_id)` to one of
/// the peer's advertised mailboxes and deposit `ciphertext`, walking the list
/// on failure. Deletes the outbox row + emits `DeliveryStatusChanged` on
/// success; leaves the (now mailbox-kind) row for the sweeper on failure.
/// Non-generic so the per-peer actor and the sweeper can call it without a
/// reference to the generic `DeliveryHub<S>`.
pub(crate) async fn run_mailbox_fallback(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    peer: PublicKey,
    message_id: MessageId,
    ciphertext: Vec<u8>,
) {
    // 1. Look up peer's mailboxes.
    let mailbox_repo = MailboxRepo::new(pool);
    let onions = match mailbox_repo.list_for_contact(&peer) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "skattr::delivery::hub",
                error = %e,
                "fallback: list_for_contact failed"
            );
            return;
        }
    };
    if onions.is_empty() {
        tracing::debug!(
            target: "skattr::delivery::hub",
            "fallback: peer has no advertised mailboxes; leaving outbox row untouched"
        );
        return;
    }

    // 2. Find the existing direct outbox row (orchestrator MOVES, never duplicates).
    let outbox = OutboxRepo::new(pool);
    let row_id = match outbox.find_direct_id(&peer.0, &message_id.0) {
        Ok(Some(id)) => id,
        Ok(None) => {
            tracing::debug!(
                target: "skattr::delivery::hub",
                "fallback: no direct outbox row to retarget"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                target: "skattr::delivery::hub",
                error = %e,
                "fallback: find_direct_id failed"
            );
            return;
        }
    };

    // 3. Pick a primary index, then walk sequentially.
    let n = onions.len();
    let primary = pick_first_mailbox_index(&message_id, n);
    let recipient_hash = recipient_hash_from_pubkey(&peer.0);
    let mut last_err: Option<crate::error::CoreError> = None;

    for offset in 0..n {
        let idx = (primary + offset) % n;
        let onion = &onions[idx];

        // 3a. Ensure a 'theirs' row exists for this onion → mailbox_id.
        let now = crate::daemon::clock::now_unix_seconds();
        let mailbox_id = match mailbox_repo.ensure_theirs(onion, now) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    target: "skattr::delivery::hub",
                    error = %e,
                    "fallback: ensure_theirs failed; trying next mailbox"
                );
                last_err = Some(e);
                continue;
            }
        };

        // 3b. Retarget the existing outbox row to this mailbox.
        //
        // The row is flipped to mailbox-kind here, before the deposit + delete
        // below. A mailbox-outbox sweeper tick landing in that window observes
        // the now-due mailbox-kind row and may deposit the same payload a second
        // time. This is benign: the recipient dedups on `(sender, envelope_id)`
        // in the same transaction as the message insert, so a second fetch
        // resolves to `Duplicate` (no second row, no second event). The only
        // cost is one extra ciphertext briefly resident on the semi-trusted
        // mailbox until fetched/deleted.
        if let Err(e) = outbox.set_mailbox_target(row_id, mailbox_id) {
            tracing::warn!(
                target: "skattr::delivery::hub",
                error = %e,
                "fallback: set_mailbox_target failed; trying next mailbox"
            );
            last_err = Some(e);
            continue;
        }

        // 3c. Connect + deposit.
        let mut client = match shared.factory.connect(onion).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    target: "skattr::delivery::hub",
                    error = %e,
                    "fallback: connect failed; trying next mailbox"
                );
                last_err = Some(e);
                continue;
            }
        };

        match client
            .deposit(recipient_hash, ciphertext.clone(), FALLBACK_TTL_SECS)
            .await
        {
            Ok(_ok) => {
                // 4. Success: delete the outbox row + emit event.
                if let Err(e) = outbox.delete_by_id(row_id) {
                    tracing::warn!(
                        target: "skattr::delivery::hub",
                        error = %e,
                        "fallback: delete_by_id after deposit failed"
                    );
                }
                let _ = shared.events.send(Event::DeliveryStatusChanged {
                    message: message_id,
                    status: DeliveryStatus::Deposited,
                });
                tracing::debug!(
                    target: "skattr::delivery::hub",
                    "fallback: deposit succeeded"
                );
                return;
            }
            Err(e) => {
                tracing::debug!(
                    target: "skattr::delivery::hub",
                    error = %e,
                    "fallback: deposit failed; trying next mailbox"
                );
                last_err = Some(e);
                continue;
            }
        }
    }

    // 5. All mailboxes exhausted. Leave outbox row in place.
    if let Some(e) = last_err {
        tracing::info!(
            target: "skattr::delivery::hub",
            error = %e,
            mailboxes = n,
            "fallback: all mailboxes failed; outbox row retained"
        );
    }
}

/// Deposit a single already-mailbox-kind outbox row, walking the peer's
/// advertised mailbox list; delete the row + emit `DeliveryStatusChanged` on a
/// successful deposit, reschedule with backoff on failure. Returns true on a
/// successful deposit. Non-generic so the sweeper can call it without a
/// reference to the generic `DeliveryHub<S>`.
pub(crate) async fn redeposit_mailbox_row(
    pool: &Pool,
    shared: &MailboxFallbackShared,
    row: &crate::storage::outbox::OutboxRow,
    now: i64,
) -> bool {
    if row.target.len() != 32 {
        tracing::warn!(
            target: "skattr::delivery::sweeper",
            row_id = row.id,
            "redeposit: skipping outbox row with malformed target (len != 32)"
        );
        reschedule_with_backoff(pool, row.id, row.attempts, now);
        return false;
    }
    let mut peer_bytes = [0u8; 32];
    peer_bytes.copy_from_slice(&row.target);
    let peer = PublicKey(peer_bytes);
    let onions = match MailboxRepo::new(pool).list_for_contact(&peer) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            reschedule_with_backoff(pool, row.id, row.attempts, now);
            return false;
        }
    };
    let recipient_hash = recipient_hash_from_pubkey(&peer.0);
    let mut mid = [0u8; 16];
    mid.copy_from_slice(&row.message_id);
    for onion in &onions {
        let mut client = match shared.factory.connect(onion).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(target: "skattr::delivery::sweeper", error = %e, "redeposit: connect failed; next mailbox");
                continue;
            }
        };
        match client
            .deposit(recipient_hash, row.payload.clone(), FALLBACK_TTL_SECS)
            .await
        {
            Ok(_ok) => {
                if let Err(e) = OutboxRepo::new(pool).delete_by_id(row.id) {
                    tracing::warn!(target: "skattr::delivery::sweeper", error = %e, "redeposit: delete_by_id failed");
                }
                let _ = shared.events.send(Event::DeliveryStatusChanged {
                    message: MessageId(mid),
                    status: DeliveryStatus::Deposited,
                });
                return true;
            }
            Err(e) => {
                tracing::debug!(target: "skattr::delivery::sweeper", error = %e, "redeposit: deposit failed; next mailbox");
                continue;
            }
        }
    }
    reschedule_with_backoff(pool, row.id, row.attempts, now);
    false
}

fn reschedule_with_backoff(pool: &Pool, row_id: i64, attempts: u32, now: i64) {
    use crate::delivery::backoff::backoff;
    let next = now.saturating_add(i64::try_from(backoff(attempts).as_millis()).unwrap_or(i64::MAX));
    if let Err(e) = OutboxRepo::new(pool).reschedule(row_id, next) {
        tracing::warn!(target: "skattr::delivery::sweeper", error = %e, "redeposit: reschedule failed");
    }
}

impl<S> Drop for DeliveryHub<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn drop(&mut self) {
        self.sweep.abort();
    }
}

/// Pick a mailbox index deterministically from a message id.
///
/// `mailboxes[blake2s(message_id) % len]` — single hash per message, so
/// retries of the same message stay pinned to the same primary while
/// different messages fan out across the list. Caller asserts `n > 0`.
fn pick_first_mailbox_index(message_id: &MessageId, n: usize) -> usize {
    use blake2::{Blake2s256, Digest};
    debug_assert!(n > 0, "pick_first_mailbox_index requires n > 0");
    let h = Blake2s256::digest(message_id.0);
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&h[0..8]);
    let v = u64::from_le_bytes(bytes) as usize;
    v % n.max(1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contact::card::{ContactCard, ContactCardBody};
    use crate::contact::Contact;
    use crate::identity::{IdentityKey, Signature};
    use crate::mailbox::client::MailboxClient;
    use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
    use crate::mailbox::poll::MailboxStream;
    use crate::mailbox::protocol::{DepositOk, ErrorBody, ErrorCode};
    use crate::storage::ContactRepo;
    use crate::transport::noise::{handshake_initiator, handshake_responder};
    use futures::{SinkExt, StreamExt};
    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex as StdMutex;
    use tokio_util::codec::Framed;

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

    // ─── Task 20: ensure_mailbox_fallback orchestrator tests ───────────

    /// Per-onion deposit outcome the test wants the in-process server to
    /// produce.
    #[derive(Clone)]
    enum DepositReply {
        Ok,
        Error(ErrorCode),
    }

    /// Stub `MailboxConnectFactory`: per-onion, hands out one in-process
    /// duplex peer with a tiny inline server that replies to one Deposit
    /// with the configured outcome. After the configured stream is
    /// consumed, subsequent connects to that onion fail with `Unreachable`.
    struct StubFactory {
        // `onion -> queue of pre-spawned client streams` and the matching
        // server-task handles. The server tasks are spawned eagerly when
        // the test calls `seed`.
        slots: StdMutex<StdHashMap<String, Vec<tokio::io::DuplexStream>>>,
        // Track which onions were `connect`-ed for assertion in tests.
        connects: StdMutex<Vec<String>>,
    }

    impl StubFactory {
        fn new() -> Self {
            Self {
                slots: StdMutex::new(StdHashMap::new()),
                connects: StdMutex::new(Vec::new()),
            }
        }

        /// Seed one Deposit-handling server for `onion`. Returns the
        /// `JoinHandle` so the test can `.await` it.
        fn seed(&self, onion: &str, reply: DepositReply) -> tokio::task::JoinHandle<()> {
            let (a, b) = tokio::io::duplex(64 * 1024);
            self.slots
                .lock()
                .unwrap()
                .entry(onion.to_string())
                .or_default()
                .push(a);
            tokio::spawn(deposit_server(b, reply))
        }

        fn connects(&self) -> Vec<String> {
            self.connects.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl MailboxConnectFactory for StubFactory {
        async fn connect(&self, onion: &str) -> Result<MailboxClient<Box<dyn MailboxStream>>> {
            self.connects.lock().unwrap().push(onion.to_string());
            let stream_opt = {
                let mut slots = self.slots.lock().unwrap();
                slots.get_mut(onion).and_then(|v| v.pop())
            };
            match stream_opt {
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

    /// Tiny inline mailbox server: read one Deposit, reply with the
    /// configured outcome, then exit.
    async fn deposit_server(server: tokio::io::DuplexStream, reply: DepositReply) {
        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        let req = framed.next().await;
        let Some(Ok(MailboxFrame::Deposit(_))) = req else {
            return;
        };
        let outgoing = match reply {
            DepositReply::Ok => MailboxFrame::DepositOk(DepositOk {
                deposit_id: [0xAB; 16],
                expires_at: 9_999,
            }),
            DepositReply::Error(code) => MailboxFrame::Error(ErrorBody {
                code,
                message: "stub".into(),
            }),
        };
        let _ = framed.send(outgoing).await;
    }

    /// Insert a contact + a card listing `mailboxes`. The signature is
    /// `[0u8; 64]` since the storage layer never re-verifies (matches
    /// existing patterns in `storage::mailboxes` tests).
    fn seed_contact_with_card(pool: &Pool, peer: PublicKey, mailboxes: Vec<String>) {
        let contacts = ContactRepo::new(pool);
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
                    mailboxes,
                    version: 1,
                    expires_at: 9_999_999_999,
                },
                signature: Signature([0u8; 64]),
            })
            .unwrap();
    }

    /// Insert a direct outbox row for `(peer, mid)` with payload `ct`.
    fn seed_direct_outbox_row(pool: &Pool, peer: &PublicKey, mid: &MessageId, ct: &[u8]) {
        let repo = OutboxRepo::new(pool);
        let outcome = repo.insert_direct(&peer.0, &mid.0, ct, 0).unwrap();
        assert!(matches!(
            outcome,
            crate::storage::outbox::InsertOutcome::Inserted
        ));
    }

    #[tokio::test]
    async fn ensure_mailbox_fallback_picks_one_then_succeeds() {
        let pool = Arc::new(Pool::in_memory());
        let peer = PublicKey([0x42; 32]);
        let mid = MessageId([0x77; 16]);
        let ct = vec![0xDE, 0xAD, 0xBE, 0xEF];

        // Two mailboxes; we don't know in advance which is primary, so
        // we seed BOTH with DepositOk. This is robust to changes in the
        // hash, while still asserting that fallback succeeds, deletes
        // the row, and emits the event.
        seed_contact_with_card(&pool, peer, vec!["mb1.onion".into(), "mb2.onion".into()]);
        seed_direct_outbox_row(&pool, &peer, &mid, &ct);

        let factory = Arc::new(StubFactory::new());
        let s1 = factory.seed("mb1.onion", DepositReply::Ok);
        let s2 = factory.seed("mb2.onion", DepositReply::Ok);

        let (events_tx, mut events_rx) = broadcast::channel::<Event>(8);
        let identity = Arc::new(IdentityKey::generate().unwrap());

        let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new_with_mailbox_fallback(
            pool.clone(),
            None,
            events_tx,
            factory.clone(),
            identity,
        );

        hub.ensure_mailbox_fallback(peer, mid, ct.clone()).await;

        // Outbox row should be gone.
        let after = OutboxRepo::new(&pool)
            .find_direct_id(&peer.0, &mid.0)
            .unwrap();
        assert!(after.is_none(), "outbox row deleted on success");

        // Mailbox row for the *primary* exists with role='theirs'.
        let primary_idx = pick_first_mailbox_index(&mid, 2);
        let onions = ["mb1.onion", "mb2.onion"];
        let chosen = onions[primary_idx];
        let row_id = pool
            .with(|c| {
                c.query_row(
                    "SELECT id FROM mailboxes WHERE onion = ?1 AND role = 'theirs'",
                    rusqlite::params![chosen],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        format!("test: {e}"),
                    ))
                })
            })
            .unwrap();
        assert!(row_id > 0);

        // Event was emitted.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        match evt {
            Event::DeliveryStatusChanged {
                message,
                status: DeliveryStatus::Deposited,
            } => assert_eq!(message, mid),
            other => panic!("unexpected event: {other:?}"),
        }

        // The orchestrator only connects to the primary on success, so
        // one of the two seeded server tasks completes; the other is
        // still parked waiting for a Deposit. Drop the factory to close
        // the unused stream and let the parked server task drain.
        drop(factory);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), s1).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), s2).await;
    }

    #[tokio::test]
    async fn ensure_mailbox_fallback_cascades_on_first_mailbox_error() {
        let pool = Arc::new(Pool::in_memory());
        let peer = PublicKey([0x42; 32]);
        let mid = MessageId([0xC0; 16]);
        let ct = vec![1u8, 2, 3, 4];

        seed_contact_with_card(&pool, peer, vec!["mb1.onion".into(), "mb2.onion".into()]);
        seed_direct_outbox_row(&pool, &peer, &mid, &ct);

        // Determine which mailbox is primary, fail it, succeed the other.
        let primary_idx = pick_first_mailbox_index(&mid, 2);
        let onions = ["mb1.onion", "mb2.onion"];
        let primary = onions[primary_idx];
        let secondary = onions[(primary_idx + 1) % 2];

        let factory = Arc::new(StubFactory::new());
        let s_pri = factory.seed(primary, DepositReply::Error(ErrorCode::RateLimited));
        let s_sec = factory.seed(secondary, DepositReply::Ok);

        let (events_tx, mut events_rx) = broadcast::channel::<Event>(8);
        let identity = Arc::new(IdentityKey::generate().unwrap());

        let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new_with_mailbox_fallback(
            pool.clone(),
            None,
            events_tx,
            factory.clone(),
            identity,
        );

        hub.ensure_mailbox_fallback(peer, mid, ct.clone()).await;

        // Outbox row gone (deposit eventually succeeded).
        assert!(OutboxRepo::new(&pool)
            .find_direct_id(&peer.0, &mid.0)
            .unwrap()
            .is_none());

        // Cascade visited primary first, then secondary.
        let connects = factory.connects();
        assert_eq!(connects.len(), 2, "both mailboxes visited");
        assert_eq!(connects[0], primary);
        assert_eq!(connects[1], secondary);

        // Deposited event emitted.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        assert!(matches!(
            evt,
            Event::DeliveryStatusChanged {
                status: DeliveryStatus::Deposited,
                ..
            }
        ));

        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), s_pri).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), s_sec).await;
    }

    #[tokio::test]
    async fn ensure_mailbox_fallback_with_no_mailboxes_leaves_outbox_row() {
        let pool = Arc::new(Pool::in_memory());
        let peer = PublicKey([0x42; 32]);
        let mid = MessageId([0x99; 16]);
        let ct = vec![9u8, 9, 9];

        // Contact exists but has NO card.
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
        seed_direct_outbox_row(&pool, &peer, &mid, &ct);

        let factory = Arc::new(StubFactory::new());
        let (events_tx, mut events_rx) = broadcast::channel::<Event>(4);
        let identity = Arc::new(IdentityKey::generate().unwrap());

        let hub: DeliveryHub<tokio::io::DuplexStream> = DeliveryHub::new_with_mailbox_fallback(
            pool.clone(),
            None,
            events_tx,
            factory.clone(),
            identity,
        );

        hub.ensure_mailbox_fallback(peer, mid, ct.clone()).await;

        // Outbox row still present.
        let after = OutboxRepo::new(&pool)
            .find_direct_id(&peer.0, &mid.0)
            .unwrap();
        assert!(after.is_some(), "outbox row preserved when no mailboxes");

        // No 'theirs' mailbox row created (none to insert).
        let theirs: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM mailboxes WHERE role = 'theirs'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        format!("test: {e}"),
                    ))
                })
            })
            .unwrap();
        assert_eq!(theirs, 0);

        // No event emitted.
        let evt =
            tokio::time::timeout(std::time::Duration::from_millis(200), events_rx.recv()).await;
        assert!(evt.is_err(), "no event when no mailboxes");
        assert!(factory.connects().is_empty());
    }

    #[test]
    fn pick_first_mailbox_index_is_deterministic() {
        let mid = MessageId([0x42; 16]);
        let a = pick_first_mailbox_index(&mid, 5);
        let b = pick_first_mailbox_index(&mid, 5);
        assert_eq!(a, b);
        assert!(a < 5);
    }

    #[test]
    fn pick_first_mailbox_index_distributes_across_messages() {
        // Sanity: not all message ids hash to the same index for n=4.
        let mut seen = std::collections::HashSet::new();
        for byte in 0u8..32 {
            let mid = MessageId([byte; 16]);
            seen.insert(pick_first_mailbox_index(&mid, 4));
        }
        assert!(seen.len() >= 2, "hash should distribute (got {seen:?})");
    }
}
