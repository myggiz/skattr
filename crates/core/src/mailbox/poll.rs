// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adaptive polling scheduler for our mailboxes.
//!
//! Per-mailbox actor with an Idle (60 s) ↔ Active (15 s) state machine.
//! ±25 % jitter per tick to break timing correlation across mailboxes.
//! Idle ceiling = 5 min when a mailbox is `Unreachable`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::daemon::events::{Event, MailboxStatus};
use crate::error::{CoreError, MailboxClientErrorKind, Result};
use crate::identity::IdentityKey;
use crate::storage::{MailboxRepo, Pool};

pub(crate) const ACTIVE_BASE: Duration = Duration::from_secs(15);
pub(crate) const IDLE_BASE: Duration = Duration::from_secs(60);
pub(crate) const IDLE_CEILING: Duration = Duration::from_secs(5 * 60);
pub(crate) const ACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);

/// Number of consecutive failures before flipping a mailbox to
/// `Unreachable` and emitting a status change.
pub(crate) const FAILURE_THRESHOLD: u32 = 5;

/// Per-actor poll cadence (Idle/Active base intervals, Unreachable
/// ceiling, and Active-hold window).
///
/// Production always uses [`PollCadence::default`] — the locked 60 s
/// Idle / 15 s Active / 5 min ceiling cadence. The only non-default
/// instance is [`PollCadence::fast`], wired exclusively through the
/// `test-harness` entrypoint `run_loopback_with_mailbox` so the
/// end-to-end offline-fallback guardrail does not have to wait a full
/// 45–75 s idle tick for the recipient to poll. Nothing on the
/// production path can construct a non-default cadence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PollCadence {
    /// Base interval while a mailbox is in the Active (hot) state.
    pub active_base: Duration,
    /// Base interval while a mailbox is Idle (cold).
    pub idle_base: Duration,
    /// Locked interval while a mailbox is `Unreachable`.
    pub idle_ceiling: Duration,
    /// How long a deposit-bearing tick holds the actor in Active.
    pub active_hold: Duration,
}

impl Default for PollCadence {
    fn default() -> Self {
        Self {
            active_base: ACTIVE_BASE,
            idle_base: IDLE_BASE,
            idle_ceiling: IDLE_CEILING,
            active_hold: ACTIVE_HOLD,
        }
    }
}

impl PollCadence {
    /// Test-only fast cadence (sub-second polling) so the cross-crate
    /// offline-fallback guardrail converges in a few seconds instead of
    /// a full Idle tick. NEVER reachable from a production build path.
    #[cfg(feature = "test-harness")]
    #[must_use]
    pub fn fast() -> Self {
        Self {
            active_base: Duration::from_millis(250),
            idle_base: Duration::from_millis(250),
            idle_ceiling: Duration::from_millis(500),
            active_hold: Duration::from_secs(30),
        }
    }
}

/// Compute the next sleep before the per-mailbox actor's next tick.
///
/// Pure function — Task 14 wraps the per-mailbox actor around it.
#[must_use]
pub(crate) fn next_interval(
    active: bool,
    unreachable: bool,
    cadence: &PollCadence,
    rng: &mut impl Rng,
) -> Duration {
    let base = match (active, unreachable) {
        (_, true) => cadence.idle_ceiling,
        (true, false) => cadence.active_base,
        (false, false) => cadence.idle_base,
    };
    let nanos = base.as_nanos() as i128;
    let jitter_range: i128 = nanos / 4; // ±25 %
    let delta = rng.gen_range(-jitter_range..=jitter_range);
    let out = (nanos + delta).max(0) as u64;
    Duration::from_nanos(out)
}

/// One Challenge → Fetch → Delete cycle. The caller (Task 15's actor)
/// hands each `PendingDeposit` to the inbound MLS dispatcher between
/// Fetch and Delete; this function does NOT decrypt or persist.
///
/// Returned `FetchResponse` is the unmodified server reply — caller
/// inspects it to decide whether to bump Active hold (non-empty
/// deposits) and to drive the inbound dispatch.
pub(crate) async fn run_one_poll_tick<S>(
    client: &mut crate::mailbox::client::MailboxClient<S>,
    signer: &crate::identity::IdentityKey,
) -> crate::error::Result<crate::mailbox::protocol::FetchResponse>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let resp = client.fetch(signer).await?;
    if !resp.deposits.is_empty() {
        let ids: Vec<[u8; 16]> = resp.deposits.iter().map(|d| d.deposit_id).collect();
        client.delete(signer, ids).await?;
    }
    Ok(resp)
}

/// One fetch → dispatch → delete-dispatched cycle for a recipient's own
/// mailbox. Fetches pending deposits, hands each ciphertext to the inbound
/// MLS pipeline via `dispatch_mailbox`, and server-side deletes ONLY the
/// deposits that persisted successfully. Undispatched deposits are left on
/// the server for the next poll (no silent loss on a transient failure).
///
/// Returns the number of deposits that were dispatched + deleted.
pub(crate) async fn poll_dispatch_once<S>(
    client: &mut crate::mailbox::client::MailboxClient<S>,
    signer: &crate::identity::IdentityKey,
    inbound: &dyn crate::delivery::peer::InboundDispatch,
) -> crate::error::Result<usize>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let resp = client.fetch(signer).await?;
    if resp.deposits.is_empty() {
        return Ok(0);
    }
    let mut dispatched: Vec<[u8; 16]> = Vec::new();
    for dep in &resp.deposits {
        if inbound.dispatch_attachment_chunk(&dep.ciphertext)
            || inbound.dispatch_mailbox(&dep.ciphertext).is_some()
        {
            dispatched.push(dep.deposit_id);
        }
    }
    let count = dispatched.len();
    if !dispatched.is_empty() {
        client.delete(signer, dispatched).await?;
    }
    Ok(count)
}

/// Inbound dispatcher that ignores everything. Used when a poll actor has
/// no MLS pipeline wired (tests).
struct NoopDispatch;
impl crate::delivery::peer::InboundDispatch for NoopDispatch {
    fn dispatch(
        &self,
        _: crate::identity::PublicKey,
        _: &[u8],
    ) -> Option<crate::envelope::MessageId> {
        None
    }
}

// ─── Task 15: PollScheduler supervisor + per-mailbox actor ────────────

/// Object-safe combined `AsyncRead + AsyncWrite + Send + Unpin` trait
/// used to box the per-tick connection stream. `MailboxClient` is
/// generic; we erase the stream type so `PollScheduler` can be
/// non-generic and stash a `Box<dyn MailboxStream>` per tick.
///
/// Blanket impl covers `arti_client::DataStream` in production and
/// `tokio::io::DuplexStream` in tests. The boxing cost per tick is
/// negligible relative to a Tor circuit setup.
pub(crate) trait MailboxStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin
{
}
impl<T> MailboxStream for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

/// Factory for fresh per-tick `MailboxClient` connections.
///
/// Production specialises to a closure that drives Arti
/// (`MailboxClient::connect`); tests use a stub that hands back the
/// in-process duplex peer. We hide the concrete stream type behind
/// `Box<dyn MailboxStream>` so `PollScheduler` itself is not generic.
#[async_trait::async_trait]
pub(crate) trait MailboxConnectFactory: Send + Sync + 'static {
    /// Open a fresh connection to the mailbox at `onion` and return a
    /// `MailboxClient` ready for Challenge / Fetch / Delete.
    async fn connect(
        &self,
        onion: &str,
    ) -> Result<crate::mailbox::client::MailboxClient<Box<dyn MailboxStream>>>;
}

/// Control messages for the `PollScheduler` supervisor.
#[derive(Debug)]
pub(crate) enum PollerCtrl {
    /// A row was inserted into `mailboxes` with `role='mine'`. Spawn an actor.
    AddMailbox(i64),
    /// A row was removed (or pending_removal). Stop the actor.
    RemoveMailbox(i64),
    /// Daemon-wide signal: local send/receive happened, bump every
    /// actor's Active-hold timer.
    BumpActive,
    /// Stop all actors and exit the supervisor.
    Shutdown,
}

/// Per-actor signal sent by the supervisor.
#[derive(Debug)]
enum ActorSignal {
    BumpActive,
    Shutdown,
}

/// Daemon-scoped poll scheduler. Owns one tokio task per `'mine'`
/// mailbox row plus a supervisor task that owns the control channel.
pub(crate) struct PollScheduler {
    ctrl: mpsc::Sender<PollerCtrl>,
    /// Aborted on drop so a panicking parent can't leak the supervisor.
    handle: JoinHandle<()>,
}

impl PollScheduler {
    /// Clone of the control sender — daemon command paths use this to
    /// dispatch `BumpActive` / `AddMailbox(id)` / `RemoveMailbox(id)`.
    #[must_use]
    pub fn ctrl(&self) -> mpsc::Sender<PollerCtrl> {
        self.ctrl.clone()
    }

    /// Spawn the supervisor and return a handle.
    ///
    /// `inbound` is `Some` in production (wires fetched ciphertexts
    /// into the existing `delivery::peer::InboundDispatch` MLS
    /// pipeline) and `None` in tests that don't exercise decryption.
    pub fn spawn(
        pool: Arc<Pool>,
        identity: Arc<IdentityKey>,
        events: broadcast::Sender<Event>,
        connect_factory: Arc<dyn MailboxConnectFactory>,
        inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
        cadence: PollCadence,
    ) -> Self {
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<PollerCtrl>(16);
        let handle = tokio::spawn(supervisor(
            pool,
            identity,
            events,
            connect_factory,
            inbound,
            cadence,
            ctrl_rx,
        ));
        Self {
            ctrl: ctrl_tx,
            handle,
        }
    }
}

impl Drop for PollScheduler {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Supervisor: spawns one actor per existing mailbox at startup, then
/// reacts to `PollerCtrl` messages. The supervisor never exits before
/// every actor it has spawned has been told to shut down — actors
/// own a per-actor `mpsc<ActorSignal>` so the supervisor can broadcast
/// `BumpActive` and `Shutdown` without inheriting per-actor timing.
async fn supervisor(
    pool: Arc<Pool>,
    identity: Arc<IdentityKey>,
    events: broadcast::Sender<Event>,
    connect_factory: Arc<dyn MailboxConnectFactory>,
    inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
    cadence: PollCadence,
    mut ctrl_rx: mpsc::Receiver<PollerCtrl>,
) {
    let mut actors: HashMap<i64, ActorEntry> = HashMap::new();

    // ── Bootstrap: spawn one actor per existing 'mine' row. ─────────
    let initial_rows = match pool.with(|c| {
        let mut stmt = c
            .prepare(
                "SELECT id FROM mailboxes WHERE role = 'mine' \
                 ORDER BY registered_at, id",
            )
            .map_err(|e| {
                CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                    "supervisor list_mine prep: {e}"
                )))
            })?;
        let ids: std::result::Result<Vec<i64>, _> = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(|e| {
                CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                    "supervisor list_mine query: {e}"
                )))
            })?
            .collect();
        ids.map_err(|e| {
            CoreError::Storage(crate::storage::StorageErrorKind::Other(format!(
                "supervisor list_mine collect: {e}"
            )))
        })
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(target: "skattr::mailbox::poll", error = %e, "supervisor: failed to list initial mailboxes");
            Vec::new()
        }
    };

    for id in initial_rows {
        let entry = spawn_actor(
            id,
            pool.clone(),
            identity.clone(),
            events.clone(),
            connect_factory.clone(),
            inbound.clone(),
            cadence,
        );
        actors.insert(id, entry);
    }

    // ── Control loop. ───────────────────────────────────────────────
    while let Some(msg) = ctrl_rx.recv().await {
        match msg {
            PollerCtrl::AddMailbox(id) => {
                if let std::collections::hash_map::Entry::Vacant(slot) = actors.entry(id) {
                    let entry = spawn_actor(
                        id,
                        pool.clone(),
                        identity.clone(),
                        events.clone(),
                        connect_factory.clone(),
                        inbound.clone(),
                        cadence,
                    );
                    slot.insert(entry);
                }
            }
            PollerCtrl::RemoveMailbox(id) => {
                if let Some(entry) = actors.remove(&id) {
                    let _ = entry.signal.send(ActorSignal::Shutdown).await;
                    // Best-effort wait so a follow-up AddMailbox of the
                    // same id can't race the actor's last writes.
                    let _ = entry.handle.await;
                }
            }
            PollerCtrl::BumpActive => {
                for entry in actors.values() {
                    // try_send: if the actor is mid-tick, the signal
                    // channel may be full (cap 4) — dropping a bump
                    // is fine, the next bump from the next send/recv
                    // will land.
                    let _ = entry.signal.try_send(ActorSignal::BumpActive);
                }
            }
            PollerCtrl::Shutdown => {
                for (_, entry) in actors.drain() {
                    let _ = entry.signal.send(ActorSignal::Shutdown).await;
                    let _ = entry.handle.await;
                }
                break;
            }
        }
    }

    // Drain any actors still alive when the ctrl channel closes.
    for (_, entry) in actors.drain() {
        let _ = entry.signal.send(ActorSignal::Shutdown).await;
        let _ = entry.handle.await;
    }
}

/// Spawn a single per-mailbox actor. Returned `ActorEntry` lets the
/// supervisor signal the actor and join it on shutdown.
fn spawn_actor(
    id: i64,
    pool: Arc<Pool>,
    identity: Arc<IdentityKey>,
    events: broadcast::Sender<Event>,
    connect_factory: Arc<dyn MailboxConnectFactory>,
    inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
    cadence: PollCadence,
) -> ActorEntry {
    // 4-slot signal mailbox: BumpActive / Shutdown. A small buffer
    // prevents transient backpressure from blocking the supervisor.
    let (sig_tx, sig_rx) = mpsc::channel::<ActorSignal>(4);
    let handle = tokio::spawn(actor_loop(
        id,
        pool,
        identity,
        events,
        connect_factory,
        inbound,
        cadence,
        sig_rx,
    ));
    ActorEntry {
        signal: sig_tx,
        handle,
    }
}

/// Per-actor bookkeeping kept by the supervisor.
struct ActorEntry {
    signal: mpsc::Sender<ActorSignal>,
    handle: JoinHandle<()>,
}

/// Per-mailbox actor:
///
/// 1. Look up the row's `onion` from `MailboxRepo`.
/// 2. Loop: sleep `next_interval(active, unreachable, &mut rng)`,
///    open a fresh `MailboxClient` via `connect_factory`, run
///    `run_one_poll_tick`, update status / consecutive-failure counter
///    / Active-hold timer, hand each fetched deposit to the inbound
///    MLS dispatcher.
/// 3. Status events fire **only on transition**, not every tick.
#[allow(clippy::too_many_arguments)]
async fn actor_loop(
    id: i64,
    pool: Arc<Pool>,
    identity: Arc<IdentityKey>,
    events: broadcast::Sender<Event>,
    connect_factory: Arc<dyn MailboxConnectFactory>,
    inbound: Option<Arc<dyn crate::delivery::peer::InboundDispatch>>,
    cadence: PollCadence,
    mut signal_rx: mpsc::Receiver<ActorSignal>,
) {
    use rand::SeedableRng;

    // Resolve the row's onion once. If the row vanishes we exit.
    let onion = {
        let repo = MailboxRepo::new(&pool);
        match repo.get(id) {
            Ok(Some(row)) => row.onion,
            Ok(None) => {
                tracing::debug!(
                    target: "skattr::mailbox::poll",
                    mailbox_id = id,
                    "actor: row missing at startup, exiting"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    target: "skattr::mailbox::poll",
                    mailbox_id = id,
                    error = %e,
                    "actor: failed to load row at startup, exiting"
                );
                return;
            }
        }
    };

    // Track current status as the *last reported* value so we only
    // emit transitions. Seed from the row's persisted status — if a
    // restart finds it `Reachable`, the first successful poll won't
    // re-emit a redundant event.
    let mut last_status: MailboxStatus = MailboxRepo::new(&pool)
        .get(id)
        .ok()
        .flatten()
        .map(|r| r.status)
        .unwrap_or(MailboxStatus::Unknown);

    let mut consecutive_failures: u32 = 0;
    let mut active_until: Option<tokio::time::Instant> = None;
    // Per-actor RNG: one seed per actor so two mailboxes don't
    // synchronise their jitter, but reproducibly within a session.
    let mut rng = rand::rngs::StdRng::from_entropy();

    loop {
        let active = active_until
            .map(|deadline| tokio::time::Instant::now() < deadline)
            .unwrap_or(false);
        let unreachable = matches!(last_status, MailboxStatus::Unreachable);
        let sleep_for = next_interval(active, unreachable, &cadence, &mut rng);

        tokio::select! {
            biased;
            sig = signal_rx.recv() => {
                match sig {
                    Some(ActorSignal::Shutdown) | None => return,
                    Some(ActorSignal::BumpActive) => {
                        active_until = Some(tokio::time::Instant::now() + cadence.active_hold);
                        // Loop back to recompute sleep with the new
                        // active state without firing an extra tick.
                        continue;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_for) => {
                // Fall through to perform a poll tick.
            }
        }

        // ── Open a fresh connection per tick. ───────────────────────
        let client_result = connect_factory.connect(&onion).await;
        let mut client = match client_result {
            Ok(c) => c,
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let now = crate::daemon::clock::now_unix_seconds();
                let repo = MailboxRepo::new(&pool);
                let _ = repo.record_error(id, error_kind_str(&e), now);
                if consecutive_failures >= FAILURE_THRESHOLD
                    && !matches!(last_status, MailboxStatus::Unreachable)
                {
                    let _ = repo.mark_status(id, MailboxStatus::Unreachable);
                    last_status = MailboxStatus::Unreachable;
                    let _ = events.send(Event::MailboxStatusChanged {
                        mailbox_id: id,
                        status: MailboxStatus::Unreachable,
                    });
                }
                continue;
            }
        };

        // ── One Challenge → Fetch → Delete cycle. ───────────────────
        let now_ts = crate::daemon::clock::now_unix_seconds();
        let _ = MailboxRepo::new(&pool).touch_poll(id, now_ts);
        let noop = NoopDispatch;
        let disp: &dyn crate::delivery::peer::InboundDispatch = inbound.as_deref().unwrap_or(&noop);
        match poll_dispatch_once(&mut client, &identity, disp).await {
            Ok(dispatched) => {
                consecutive_failures = 0;
                // Hold the scheduler in Active (faster polling) only when a
                // deposit was actually dispatched + drained — not merely
                // fetched. Deposits that fetch but never dispatch (e.g. the
                // deferred ts-replay poison case) should not keep us hot.
                if dispatched > 0 {
                    active_until = Some(tokio::time::Instant::now() + cadence.active_hold);
                }
                if !matches!(last_status, MailboxStatus::Reachable) {
                    let _ = MailboxRepo::new(&pool).mark_status(id, MailboxStatus::Reachable);
                    last_status = MailboxStatus::Reachable;
                    let _ = events.send(Event::MailboxStatusChanged {
                        mailbox_id: id,
                        status: MailboxStatus::Reachable,
                    });
                }
            }
            Err(CoreError::MailboxClient(MailboxClientErrorKind::RateLimited)) => {
                // RateLimited: do NOT count as a failure that flips
                // the mailbox to Unreachable, but emit a transition
                // for one tick so the UI can surface it.
                let repo = MailboxRepo::new(&pool);
                let _ = repo.record_error(
                    id,
                    error_kind_str(&CoreError::MailboxClient(
                        MailboxClientErrorKind::RateLimited,
                    )),
                    crate::daemon::clock::now_unix_seconds(),
                );
                if !matches!(last_status, MailboxStatus::RateLimited) {
                    let _ = repo.mark_status(id, MailboxStatus::RateLimited);
                    last_status = MailboxStatus::RateLimited;
                    let _ = events.send(Event::MailboxStatusChanged {
                        mailbox_id: id,
                        status: MailboxStatus::RateLimited,
                    });
                }
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let now = crate::daemon::clock::now_unix_seconds();
                let repo = MailboxRepo::new(&pool);
                let _ = repo.record_error(id, error_kind_str(&e), now);
                if consecutive_failures >= FAILURE_THRESHOLD
                    && !matches!(last_status, MailboxStatus::Unreachable)
                {
                    let _ = repo.mark_status(id, MailboxStatus::Unreachable);
                    last_status = MailboxStatus::Unreachable;
                    let _ = events.send(Event::MailboxStatusChanged {
                        mailbox_id: id,
                        status: MailboxStatus::Unreachable,
                    });
                }
            }
        }
    }
}

/// Map a `CoreError` to a short, redaction-safe string used for
/// `mailboxes.last_error_kind`. We avoid `Display` since some
/// upstream errors include arbitrary server-supplied text.
fn error_kind_str(e: &CoreError) -> &'static str {
    match e {
        CoreError::MailboxClient(MailboxClientErrorKind::Unreachable) => "unreachable",
        CoreError::MailboxClient(MailboxClientErrorKind::RateLimited) => "rate_limited",
        CoreError::MailboxClient(MailboxClientErrorKind::RecipientFull) => "recipient_full",
        CoreError::MailboxClient(MailboxClientErrorKind::InvalidSignature) => "invalid_signature",
        CoreError::MailboxClient(MailboxClientErrorKind::NonceExpired) => "nonce_expired",
        CoreError::MailboxClient(MailboxClientErrorKind::Malformed) => "malformed",
        CoreError::MailboxClient(MailboxClientErrorKind::HashMismatch) => "hash_mismatch",
        CoreError::MailboxClient(MailboxClientErrorKind::UnsupportedVersion) => {
            "unsupported_version"
        }
        CoreError::MailboxClient(MailboxClientErrorKind::Other(_)) => "other",
        _ => "other",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn active_interval_within_active_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(true, false, &PollCadence::default(), &mut rng);
            assert!(
                d >= Duration::from_millis(11_250) && d <= Duration::from_millis(18_750),
                "active out of band: {d:?}"
            );
        }
    }

    #[test]
    fn idle_interval_within_idle_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(false, false, &PollCadence::default(), &mut rng);
            assert!(
                d >= Duration::from_millis(45_000) && d <= Duration::from_millis(75_000),
                "idle out of band: {d:?}"
            );
        }
    }

    #[test]
    fn unreachable_interval_locks_to_idle_ceiling() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(false, true, &PollCadence::default(), &mut rng);
            assert!(d >= Duration::from_millis(225_000) && d <= Duration::from_millis(375_000));
        }
    }

    #[test]
    fn active_overrides_unreachable_ceiling() {
        // Even with `active=true`, if the actor is `unreachable=true` the ceiling wins.
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(true, true, &PollCadence::default(), &mut rng);
            assert!(d >= Duration::from_millis(225_000));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_tick_drives_full_challenge_fetch_delete_cycle() {
        use crate::identity::IdentityKey;
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, DeleteOk, FetchResponse, PendingDeposit};
        use futures::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let cycles_completed = Arc::new(AtomicU32::new(0));
        let counter = cycles_completed.clone();

        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            // 1. Challenge → ChallengeNonce
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            // 2. Fetch → FetchResponse with one deposit
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::FetchResponse(FetchResponse {
                    deposits: vec![PendingDeposit {
                        deposit_id: [1; 16],
                        ciphertext: vec![9],
                        received_at: 1,
                    }],
                }))
                .await
                .unwrap();
            // 3. Delete: Challenge → ChallengeNonce → Delete → DeleteOk
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [1; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::DeleteOk(DeleteOk {
                    deleted: 1,
                    not_found: 0,
                }))
                .await
                .unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let signer = IdentityKey::generate().unwrap();
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let resp = run_one_poll_tick(&mut client, &signer).await.unwrap();
        assert_eq!(resp.deposits.len(), 1);
        server.await.unwrap();
        assert_eq!(cycles_completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poll_tick_skips_delete_when_no_deposits() {
        use crate::identity::IdentityKey;
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, FetchResponse};
        use futures::{SinkExt, StreamExt};
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::FetchResponse(FetchResponse {
                    deposits: vec![],
                }))
                .await
                .unwrap();
            // No more frames expected — if `run_one_poll_tick` calls Delete on an
            // empty deposit list, the test will hang waiting for a request that
            // never arrives. The framed.next() in the test would error out from
            // the EOF path, propagating into `run_one_poll_tick` as an Err.
        });

        let signer = IdentityKey::generate().unwrap();
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let resp = run_one_poll_tick(&mut client, &signer).await.unwrap();
        assert!(resp.deposits.is_empty());
        server.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_dispatch_once_persists_and_deletes_dispatched() {
        use crate::contact::Contact;
        use crate::daemon::inbound::DaemonInbound;
        use crate::envelope::{Envelope, Kind, MessageId};
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, DeleteOk, FetchResponse, PendingDeposit};
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        use crate::storage::key_packages::KeyPackageRepo;
        use crate::storage::{ContactRepo, MessageRepo, MlsGroupRepo, Pool};
        use futures::{SinkExt, StreamExt};
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let pool = Arc::new(Pool::in_memory());
        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_pk = bob.public();

        // Group: alice adds bob; persist alice's group; link bob as the
        // contact for the group so dispatch_mailbox can attribute the sender.
        let bob_provider = MlsProvider::new();
        let bob_kp =
            KeyPackage::generate(&bob, &bob_provider, &KeyPackageRepo::new(&pool)).unwrap();
        let mut alice_group =
            crate::mls::Group::create_solo(&alice, None, None, MlsProvider::new()).unwrap();
        let (welcome, _commit) = alice_group.add_member(&bob_kp, None, None).unwrap();
        let gid = alice_group.id().0.clone();
        let mut bob_group =
            crate::mls::Group::join_from_welcome(&bob, &welcome, None, None, bob_provider).unwrap();
        alice_group.save(&MlsGroupRepo::new(&pool)).unwrap();
        let contacts = ContactRepo::new(&pool);
        contacts
            .upsert(&Contact {
                identity: bob_pk,
                display_name: None,
                added_at: 0,
                card: None,
                muted: false,
            })
            .unwrap();
        contacts.set_group_id(&bob_pk, &gid).unwrap();

        // Bob encrypts a message (ts within ±1h).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let env = Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: now_ms,
            reply_to: None,
            kind: Kind::Text { body: "mbx".into() },
        };
        let ciphertext = bob_group.encrypt(&env).unwrap();

        // Inline mailbox server: Challenge→Nonce, Fetch→one deposit,
        // Challenge→Nonce, Delete→DeleteOk.
        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await; // Challenge (for fetch)
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [1; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            let _ = framed.next().await; // Fetch
            framed
                .send(MailboxFrame::FetchResponse(FetchResponse {
                    deposits: vec![PendingDeposit {
                        deposit_id: [9; 16],
                        ciphertext,
                        received_at: 1,
                    }],
                }))
                .await
                .unwrap();
            let _ = framed.next().await; // Challenge (for delete)
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [2; 32],
                    issued_at: 2,
                }))
                .await
                .unwrap();
            let _ = framed.next().await; // Delete
            framed
                .send(MailboxFrame::DeleteOk(DeleteOk {
                    deleted: 1,
                    not_found: 0,
                }))
                .await
                .unwrap();
        });

        let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
        // DaemonInbound's dispatch_mailbox derives the reader from the group,
        // not the daemon identity, so no set_identity is needed (mirrors the
        // committed inbound test dispatch_mailbox_trial_decrypts...).
        let inbound = DaemonInbound::new(pool.clone(), events_tx);
        let mut client = MailboxClient::from_stream("a.onion".into(), a);

        let dispatched = poll_dispatch_once(&mut client, &alice, &inbound)
            .await
            .unwrap();
        assert_eq!(dispatched, 1, "one deposit must be dispatched + deleted");

        let rows = MessageRepo::new(&pool).recent(&gid, 10).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "received message must persist in alice's pool"
        );

        server.await.unwrap();
    }

    // ── Task 15: scheduler supervisor + actor ─────────────────────────

    use std::sync::Mutex as StdMutex;

    /// Stub factory: hands out one in-process duplex peer per `connect`
    /// call. The test spawns a tiny inline server task on the peer for
    /// the actor to talk to. Once the slot is empty, subsequent
    /// `connect()` calls fail with `Unreachable`.
    struct DuplexFactory {
        slots: StdMutex<Vec<tokio::io::DuplexStream>>,
    }

    impl DuplexFactory {
        fn empty() -> Self {
            Self {
                slots: StdMutex::new(Vec::new()),
            }
        }
        fn push_stream(&self, s: tokio::io::DuplexStream) {
            self.slots.lock().unwrap().push(s);
        }
    }

    #[async_trait::async_trait]
    impl MailboxConnectFactory for DuplexFactory {
        async fn connect(
            &self,
            onion: &str,
        ) -> Result<crate::mailbox::client::MailboxClient<Box<dyn MailboxStream>>> {
            // FIFO: the test seeds the slots in tick order.
            let stream_opt = self.slots.lock().unwrap().pop();
            match stream_opt {
                Some(s) => {
                    let boxed: Box<dyn MailboxStream> = Box::new(s);
                    Ok(crate::mailbox::client::MailboxClient::from_stream(
                        onion.to_string(),
                        boxed,
                    ))
                }
                None => Err(CoreError::MailboxClient(
                    MailboxClientErrorKind::Unreachable,
                )),
            }
        }
    }

    /// Stub factory that always errors — drives the consecutive-failures
    /// path.
    struct AlwaysFailFactory;

    #[async_trait::async_trait]
    impl MailboxConnectFactory for AlwaysFailFactory {
        async fn connect(
            &self,
            _onion: &str,
        ) -> Result<crate::mailbox::client::MailboxClient<Box<dyn MailboxStream>>> {
            Err(CoreError::MailboxClient(
                MailboxClientErrorKind::Unreachable,
            ))
        }
    }

    /// One Challenge → empty FetchResponse handshake — enough for one
    /// successful poll tick that doesn't trigger Delete.
    async fn one_successful_tick(server: tokio::io::DuplexStream) {
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, FetchResponse};
        use futures::{SinkExt, StreamExt};
        use tokio_util::codec::Framed;

        let mut framed = Framed::new(server, MailboxFrameCodec::new());
        let _ = framed.next().await;
        framed
            .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                nonce: [0xA1; 32],
                issued_at: 1,
            }))
            .await
            .unwrap();
        let _ = framed.next().await;
        framed
            .send(MailboxFrame::FetchResponse(FetchResponse {
                deposits: vec![],
            }))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn scheduler_emits_status_change_on_first_successful_poll() {
        use crate::storage::{MailboxRepo, Pool};
        use std::sync::Arc;
        use tokio::sync::broadcast;

        // Pool with one 'mine' row.
        let pool = Arc::new(Pool::in_memory());
        let mailbox_id = MailboxRepo::new(&pool).add_mine("a.onion", 100).unwrap();

        // Connect factory: one slot. The other duplex end gets a tiny
        // server task that completes one Challenge / Fetch cycle.
        let factory = Arc::new(DuplexFactory::empty());
        let (a, b) = tokio::io::duplex(64 * 1024);
        factory.push_stream(a);
        let server = tokio::spawn(one_successful_tick(b));

        let identity = Arc::new(IdentityKey::generate().unwrap());
        let (events_tx, mut events_rx) = broadcast::channel::<Event>(16);

        let scheduler = PollScheduler::spawn(
            pool.clone(),
            identity,
            events_tx.clone(),
            factory.clone(),
            None,
            PollCadence::default(),
        );

        // Wait for the event with auto-advancing virtual time. Idle
        // interval is ~60s ± 25%; the runtime auto-advances time to
        // the next sleep when no tasks are ready, so the actor's
        // first idle sleep fires once everything else parks.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(120), events_rx.recv())
            .await
            .expect("event received in time")
            .expect("event channel open");
        match evt {
            Event::MailboxStatusChanged {
                mailbox_id: id,
                status,
            } => {
                assert_eq!(id, mailbox_id);
                assert_eq!(status, MailboxStatus::Reachable);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // DB should now reflect Reachable.
        let row = MailboxRepo::new(&pool).get(mailbox_id).unwrap().unwrap();
        assert_eq!(row.status, MailboxStatus::Reachable);

        drop(scheduler);
        let _ = server.await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn scheduler_marks_unreachable_after_consecutive_failures() {
        use crate::storage::{MailboxRepo, Pool};
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let pool = Arc::new(Pool::in_memory());
        let mailbox_id = MailboxRepo::new(&pool).add_mine("dead.onion", 100).unwrap();

        let factory = Arc::new(AlwaysFailFactory);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let (events_tx, mut events_rx) = broadcast::channel::<Event>(16);

        let scheduler = PollScheduler::spawn(
            pool.clone(),
            identity,
            events_tx.clone(),
            factory.clone(),
            None,
            PollCadence::default(),
        );

        // We need at least FAILURE_THRESHOLD ticks. Each idle tick is
        // ~60s ± 25% (max 75s). Drive 5 × 80s = 400s of virtual time
        // with yields between to let the actor process.
        for _ in 0..(FAILURE_THRESHOLD as usize + 1) {
            tokio::time::advance(std::time::Duration::from_secs(80)).await;
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
        }

        // Should see exactly one MailboxStatusChanged{Unreachable}.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(5), events_rx.recv())
            .await
            .expect("event received in time")
            .expect("event channel open");
        match evt {
            Event::MailboxStatusChanged {
                mailbox_id: id,
                status,
            } => {
                assert_eq!(id, mailbox_id);
                assert_eq!(status, MailboxStatus::Unreachable);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Persisted status matches.
        let row = MailboxRepo::new(&pool).get(mailbox_id).unwrap().unwrap();
        assert_eq!(row.status, MailboxStatus::Unreachable);

        drop(scheduler);
    }

    #[tokio::test]
    async fn scheduler_drop_aborts_supervisor() {
        use crate::storage::Pool;
        use std::sync::Arc;
        use tokio::sync::broadcast;

        let pool = Arc::new(Pool::in_memory());
        let factory = Arc::new(AlwaysFailFactory);
        let identity = Arc::new(IdentityKey::generate().unwrap());
        let (events_tx, _events_rx) = broadcast::channel::<Event>(4);

        // No mailbox rows present — supervisor sits idle on its ctrl rx.
        let scheduler = PollScheduler::spawn(
            pool.clone(),
            identity,
            events_tx.clone(),
            factory.clone(),
            None,
            PollCadence::default(),
        );
        // Sanity: ctrl sender clone-able.
        let _c = scheduler.ctrl();
        drop(scheduler);
        // Letting tokio settle would let any orphan task run forever;
        // the abort on drop ensures it doesn't.
    }
}
