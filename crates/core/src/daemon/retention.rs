// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Daemon-owned retention sweep.
//!
//! Hourly tokio task; deletes any `messages` row whose
//! `ts_daemon_recv < now - retention_days * 86400`. Respects
//! `[history] retention_days = 0` as a no-op (infinite retention).

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::clock::now_unix_seconds;
use crate::storage::messages::MessageRepo;
use crate::storage::Pool;
use crate::storage::StorageErrorKind;

/// Spawn the retention sweep on the current Tokio runtime.
///
/// `tick` is exposed for tests; production callers use
/// `Duration::from_secs(3600)`. The task exits when `shutdown` flips
/// to `true`.
pub fn spawn_sweep(
    pool: Arc<Pool>,
    retention_days: u32,
    tick: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick) => {
                    // Message-pruning step (gated on retention_days).
                    if retention_days != 0 {
                        let cutoff = now_unix_seconds()
                            .saturating_sub(i64::from(retention_days).saturating_mul(86_400));
                        match MessageRepo::new(&pool).prune_before(None, cutoff) {
                            Ok(n) if n > 0 => tracing::info!(
                                rows = n, cutoff_ts_recv = cutoff,
                                "retention sweep deleted rows"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                error = %e,
                                "retention sweep failed; will retry next tick"
                            ),
                        }
                    }

                    // Outstanding-invite expiry sweep — always runs.
                    let now = now_unix_seconds();
                    let oi = crate::storage::OutstandingInviteRepo::new(&pool);
                    match oi.purge_expired(now) {
                        Ok(n) if n > 0 => tracing::debug!(
                            rows = n,
                            "retention: purged expired outstanding invites"
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            error = %e,
                            "retention: outstanding invite purge failed"
                        ),
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::storage::messages::InsertParams;

    fn env(body: &str, ts: i64) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts,
            reply_to: None,
            kind: Kind::Text { body: body.into() },
        }
    }

    #[tokio::test]
    async fn sweep_no_op_when_retention_days_zero() {
        let pool = Arc::new(Pool::in_memory());
        MessageRepo::new(&pool)
            .insert(InsertParams {
                group_id: &[0x01; 32],
                sender: &[0u8; 32],
                envelope: &env("x", 1000),
                mls_generation: 0,
                ts_daemon_recv: 0,
            })
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(pool.clone(), 0, Duration::from_millis(20), rx);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })
            })
            .unwrap();
        assert_eq!(n, 1, "retention_days=0 must not delete anything");
    }

    #[tokio::test]
    async fn sweep_deletes_rows_older_than_cutoff() {
        let pool = Arc::new(Pool::in_memory());
        let now = now_unix_seconds();

        // Three rows: 2 days old, 1 day old, just-now.
        for (i, ts_offset) in [(-2 * 86_400, 0), (-86_400, 1), (0, 2)].iter().enumerate() {
            let _ = i;
            MessageRepo::new(&pool)
                .insert(InsertParams {
                    group_id: &[0x02; 32],
                    sender: &[0u8; 32],
                    envelope: &env("y", 1000),
                    mls_generation: 0,
                    ts_daemon_recv: now + ts_offset.0,
                })
                .unwrap();
        }

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(
            pool.clone(),
            1, /* day */
            Duration::from_millis(20),
            rx,
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        let n: i64 = pool
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })
            })
            .unwrap();
        assert!(
            n <= 2,
            "expected at most 2 rows after 1-day cutoff sweep, got {n}"
        );
    }

    #[tokio::test]
    async fn sweep_purges_expired_outstanding_invites() {
        use crate::storage::OutstandingInviteRepo;
        use zeroize::Zeroizing;

        let pool = Arc::new(Pool::in_memory());
        let now = now_unix_seconds();

        let oi = OutstandingInviteRepo::new(&pool);
        oi.put(
            &[0x10; 32],
            &Zeroizing::new([0u8; 32]),
            &[],
            now - 1,
            now - 3600,
        )
        .unwrap();
        oi.put(
            &[0x20; 32],
            &Zeroizing::new([0u8; 32]),
            &[],
            now + 3600,
            now,
        )
        .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let h = spawn_sweep(pool.clone(), 0, Duration::from_millis(20), rx);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tx.send(true);
        let _ = h.await;

        // Expired row purged; non-expired retained.
        assert!(oi.get_psk(&[0x10; 32]).unwrap().is_none());
        assert!(oi.get_psk(&[0x20; 32]).unwrap().is_some());
    }
}
