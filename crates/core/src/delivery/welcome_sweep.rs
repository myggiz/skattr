// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! #93: the sole durable first-contact Welcome-delivery path.
//!
//! Sibling to `delivery::chunk_sweep`. Reads due `pending_welcomes` rows (the
//! durable "first contact still pending" signal — a row exists ⟺ the peer has
//! not yet Ack'd its Welcome), re-sends each Welcome over the existing
//! Noise_XK transport via `DeliveryHub::send_welcome`, and awaits the peer's
//! Ack (bounded). On Ack, first contact is finalized by **deleting** the
//! `pending_welcomes` row (`GroupState` is not persisted — `Group::load`
//! always reconstructs `Active` — so row-existence, not `GroupState`, is the
//! durable pending signal) and sending the reverse-direction self-card. On
//! failure / timeout the row is rescheduled with bounded backoff.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::daemon::events::Event;
use crate::daemon::handle::DaemonHandle;
use crate::delivery::hub::DeliveryHub;
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::pending_welcomes::PendingWelcomeRepo;
use crate::storage::Pool;

/// How long we wait for the peer's Welcome Ack before treating a re-send as
/// failed and rescheduling the row. Bounded so a hung/half-open connection
/// can't wedge the sweep pass.
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Per-row backoff schedule (ms): index by `min(attempts, len-1)`. Caps at
/// ~60_000 ms so an offline inviter is retried at most once a minute.
const BACKOFF_MS: &[i64] = &[5_000, 15_000, 30_000, 60_000];

/// #107: bound first-contact Welcome re-sends. A first contact that hasn't
/// Ack'd within this age is marked failed (the sweep stops; the UI surfaces
/// "couldn't connect — remove & re-invite"), instead of retrying forever — a
/// circuit-rebind (`Psk(KeyNotFound)`) is permanent and no retry count helps,
/// while a genuinely slow peer still gets up to this long. 24 h.
const MAX_WELCOME_AGE_MS: i64 = 24 * 60 * 60 * 1_000;

/// Bounded backoff (ms) for the `attempts`-th pending-Welcome re-send.
///
/// `attempts` is the row's current attempt count (0 on the first send).
/// Caps at the last `BACKOFF_MS` entry (~60_000 ms).
pub(crate) fn welcome_backoff_ms(attempts: i64) -> i64 {
    let idx = attempts.max(0) as usize;
    BACKOFF_MS[idx.min(BACKOFF_MS.len() - 1)]
}

/// The durable first-contact-complete transition: **delete** the pending
/// Welcome row for `peer`.
///
/// After this returns, `PendingWelcomeRepo::is_pending(peer)` is `false`, so
/// `send_message` is unblocked (the group is now usable) and a second Ack is a
/// harmless no-op (deleting an absent row is idempotent). This is the ONLY
/// place the row is removed on the happy path. It does NOT touch `GroupState`
/// (not persisted — a no-op on a load-`Active` group).
pub(crate) fn finalize_welcome_ack(pool: &Pool, peer: &[u8; 32]) -> Result<()> {
    PendingWelcomeRepo::new(pool).delete(peer)?;
    tracing::info!(
        target: "skattr::delivery::welcome_sweep",
        "welcome: acked — first contact now active"
    );
    Ok(())
}

/// Thin async wrapper around [`finalize_welcome_ack`] that adds the edge I/O:
/// after the durable delete, send the reverse-direction self-card (so the peer
/// learns our onion/mailboxes now that the group is live) and emit
/// `Event::ContactUpdated(peer)`.
///
/// Idempotent: a second Ack finds no row (delete is a no-op) and simply
/// re-sends the card + re-emits the event, which is harmless.
pub(crate) async fn on_welcome_acked<S>(handle: &Arc<DaemonHandle<S>>, peer: &[u8; 32])
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if let Err(e) = finalize_welcome_ack(&handle.pool, peer) {
        tracing::warn!(
            target: "skattr::delivery::welcome_sweep",
            error = %e,
            "welcome-ack: finalize (row delete) failed"
        );
        return;
    }

    let peer_pk = PublicKey(*peer);
    // Reverse-direction self-card: best-effort. `build_self_card` needs a live
    // onion; skip quietly (log) if Tor isn't ready yet — the periodic card
    // republish will catch up.
    match crate::daemon::dispatch::build_self_card(handle) {
        Ok(card) => {
            crate::daemon::dispatch::send_card_to_contact(handle, &card, peer_pk).await;
        }
        Err(e) => {
            tracing::warn!(
                target: "skattr::delivery::welcome_sweep",
                error = ?e,
                "welcome-ack: self-card build skipped"
            );
        }
    }

    let _ = handle.events_tx.send(Event::ContactUpdated(peer_pk));
}

/// One sweep pass: re-send every due pending Welcome, await its Ack (bounded),
/// and on Ack finalize first contact; on failure / timeout reschedule the row
/// with bounded backoff.
///
/// Mirrors `chunk_sweep::run_chunk_sweep`: `due → act → reschedule`. Logs are
/// redaction-safe — counts and attempt numbers only, never a peer / onion /
/// payload.
pub(crate) async fn run_welcome_sweep<S>(
    pool: &Arc<Pool>,
    hub: &Arc<DeliveryHub<S>>,
    handle: &Arc<DaemonHandle<S>>,
    now_ms: i64,
    batch: usize,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let repo = PendingWelcomeRepo::new(pool);
    let due = match repo.due(now_ms, batch) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "skattr::delivery::welcome_sweep", error = %e, "due() failed");
            return;
        }
    };
    let n = due.len();
    if n > 0 {
        tracing::debug!(
            target: "skattr::delivery::welcome_sweep",
            due = n,
            "welcome-sweep: {n} pending welcomes due"
        );
    }

    for row in due {
        tracing::debug!(
            target: "skattr::delivery::welcome_sweep",
            attempt = row.attempts,
            "welcome-sweep: re-sending pending welcome"
        );
        let peer_pk = PublicKey(row.peer);

        // Hand the Welcome to the per-peer actor (dials on demand if no live
        // conn). A `send_welcome` error, an Err ack, or a timeout all fall
        // through to reschedule.
        let acked = match hub.send_welcome(peer_pk, row.welcome_bytes).await {
            Ok(ack_rx) => matches!(
                tokio::time::timeout(ACK_TIMEOUT, ack_rx).await,
                Ok(Ok(Ok(())))
            ),
            Err(e) => {
                tracing::debug!(
                    target: "skattr::delivery::welcome_sweep",
                    error = %e,
                    "welcome-sweep: send_welcome failed; will reschedule"
                );
                false
            }
        };

        if acked {
            on_welcome_acked(handle, &row.peer).await;
        } else if now_ms.saturating_sub(row.created_at) >= MAX_WELCOME_AGE_MS {
            tracing::warn!(
                target: "skattr::delivery::welcome_sweep",
                attempts = row.attempts,
                "welcome-sweep: first contact exceeded MAX_WELCOME_AGE; marking failed"
            );
            if let Err(e) = repo.mark_failed(&row.peer) {
                tracing::warn!(
                    target: "skattr::delivery::welcome_sweep",
                    error = %e,
                    "welcome-sweep: mark_failed failed"
                );
            }
        } else {
            let next = now_ms.saturating_add(welcome_backoff_ms(row.attempts));
            if let Err(e) = repo.reschedule(&row.peer, next) {
                tracing::warn!(
                    target: "skattr::delivery::welcome_sweep",
                    error = %e,
                    "welcome-sweep: reschedule failed"
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Build a minimal rig for sweep tests: in-memory pool, a hub with no real
    /// transport (dial always fails → ack_tx.send(Err(())) immediately → not-acked
    /// branch runs), and a DaemonHandle.
    fn sweep_rig() -> (
        Arc<crate::storage::Pool>,
        Arc<DeliveryHub<tokio::io::DuplexStream>>,
        Arc<DaemonHandle<tokio::io::DuplexStream>>,
    ) {
        let pool = Arc::new(crate::storage::Pool::in_memory());
        let hub = Arc::new(DeliveryHub::new(pool.clone()));
        let seed = crate::identity::Seed::generate().unwrap();
        let identity = crate::identity::IdentityKey::from_seed(&seed).unwrap();
        let (events_tx, _) = tokio::sync::broadcast::channel(16);
        let handle = Arc::new(DaemonHandle::new(
            pool.clone(),
            hub.clone(),
            identity,
            events_tx,
        ));
        (pool, hub, handle)
    }

    /// Over-age, never-acked: sweep must mark the row failed and stop retrying.
    #[tokio::test]
    async fn sweep_marks_failed_after_max_age() {
        let (pool, hub, handle) = sweep_rig();
        let peer = [0x22u8; 32];
        let created_at = 0i64; // far in the past
        pool.transaction(|tx| {
            crate::storage::pending_welcomes::PendingWelcomeRepo::insert_in_tx(
                tx, &peer, b"gid", b"welcome", 0, created_at,
            )
        })
        .unwrap();

        let now = MAX_WELCOME_AGE_MS + 1; // row age exceeds the cap
        run_welcome_sweep(&pool, &hub, &handle, now, 16).await;

        let repo = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&pool);
        assert!(
            repo.is_failed(&peer).unwrap(),
            "over-age unacked welcome must be marked failed"
        );
        assert!(
            repo.is_pending(&peer).unwrap(),
            "row kept so contact stays PendingJoin"
        );
        // A second pass sees nothing due (failed rows are excluded from due()).
        assert_eq!(repo.due(now + 1, 16).unwrap().len(), 0);
    }

    /// Under max age, never-acked: sweep must reschedule (not fail) the row.
    #[tokio::test]
    async fn sweep_reschedules_within_max_age() {
        let (pool, hub, handle) = sweep_rig();
        let peer = [0x33u8; 32];
        let now = 10_000i64;
        pool.transaction(|tx| {
            crate::storage::pending_welcomes::PendingWelcomeRepo::insert_in_tx(
                tx,
                &peer,
                b"gid",
                b"welcome",
                0,
                now - 1_000, // created 1 s ago — well under MAX_WELCOME_AGE_MS
            )
        })
        .unwrap();

        run_welcome_sweep(&pool, &hub, &handle, now, 16).await;

        let repo = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&pool);
        assert!(
            !repo.is_failed(&peer).unwrap(),
            "young unacked welcome must NOT be failed yet"
        );
        assert!(repo.is_pending(&peer).unwrap());
    }

    #[test]
    fn finalize_welcome_ack_deletes_row_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        let pool = crate::storage::Pool::open(dir.path(), &seed).unwrap();
        let repo = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&pool);
        let peer = [7u8; 32];
        pool.transaction(|tx| {
            crate::storage::pending_welcomes::PendingWelcomeRepo::insert_in_tx(
                tx,
                &peer,
                &[1, 2, 3],
                &[9, 9, 9],
                1_000,
                1_000,
            )
        })
        .unwrap();
        assert!(repo.is_pending(&peer).unwrap());
        finalize_welcome_ack(&pool, &peer).unwrap();
        assert!(
            !repo.is_pending(&peer).unwrap(),
            "row deleted → no longer pending"
        );
        finalize_welcome_ack(&pool, &peer).unwrap(); // idempotent, no error
    }

    /// Ordering invariant for #108: `on_welcome_acked` calls `finalize_welcome_ack`
    /// **before** sending the self-card (via `send_card_to_contact`).  The
    /// `is_peer_pending` gate in `send_card_to_contact` must therefore see the
    /// row as gone — otherwise the post-Ack card would be silently skipped.
    ///
    /// This test proves the ordering at the DB level: `finalize_welcome_ack`
    /// makes `is_pending` false before any card-send I/O would be attempted.
    /// (The live async `on_welcome_acked` is not exercised here to avoid
    /// transport dependencies; the sequential call order in its body is the
    /// contract.)
    #[test]
    fn finalize_welcome_ack_clears_pending_before_card_send_would_run() {
        let dir = tempfile::tempdir().unwrap();
        let seed = crate::identity::Seed::generate().unwrap();
        let pool = crate::storage::Pool::open(dir.path(), &seed).unwrap();
        let repo = crate::storage::pending_welcomes::PendingWelcomeRepo::new(&pool);
        let peer = [0x42u8; 32];

        pool.transaction(|tx| {
            crate::storage::pending_welcomes::PendingWelcomeRepo::insert_in_tx(
                tx,
                &peer,
                &[1u8; 32],
                &[0xAB, 0xCD],
                5_000,
                0,
            )
        })
        .unwrap();

        // Gate is ON before ack.
        assert!(
            repo.is_pending(&peer).unwrap(),
            "row present → is_pending must be true before ack"
        );

        // `on_welcome_acked` calls `finalize_welcome_ack` first; simulate that.
        finalize_welcome_ack(&pool, &peer).unwrap();

        // Gate is OFF immediately after — the card-send path sees no pending row.
        assert!(
            !repo.is_pending(&peer).unwrap(),
            "#108 ordering: is_pending must be false after finalize so send_card_to_contact is not gated out"
        );
    }

    #[test]
    fn welcome_backoff_caps_at_60s() {
        assert_eq!(welcome_backoff_ms(0), 5_000);
        assert_eq!(welcome_backoff_ms(1), 15_000);
        assert_eq!(welcome_backoff_ms(2), 30_000);
        assert_eq!(welcome_backoff_ms(3), 60_000);
        // Saturates at the last bucket for any higher attempt count.
        assert_eq!(welcome_backoff_ms(4), 60_000);
        assert_eq!(welcome_backoff_ms(99), 60_000);
        // Negative (shouldn't happen) is clamped to the first bucket.
        assert_eq!(welcome_backoff_ms(-1), 5_000);
    }
}
