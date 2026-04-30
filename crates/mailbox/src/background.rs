// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Periodic background tasks: deposit expiry, challenge nonce
//! sweep, and aggregate metrics emission.
//!
//! Spawned by the binary's `main.rs` once per server instance. Each
//! task runs forever until its `CancellationToken` is fired; the
//! shared owners (`Store`, `Challenges`) live in [`MailboxServer`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::auth::Challenges;
use crate::store::Store;

/// Cadence of the deposit expiry sweep.
pub const EXPIRE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Cadence of the challenge-nonce sweep.
pub const CHALLENGE_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Cadence of the metrics tick.
pub const METRICS_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn the deposit-expiry sweep loop. Returns the join handle so
/// the binary can `await` it during shutdown.
#[must_use]
pub fn spawn_expire_sweep(
    store: Arc<Store>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(EXPIRE_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let now = now_unix_secs();
                    match store.expire_sweep(now) {
                        Ok(n) if n > 0 => info!(expired = n, "deposit expiry sweep"),
                        Ok(_) => {}
                        Err(e) => tracing::error!(?e, tag = "expire_sweep_failed"),
                    }
                }
            }
        }
    })
}

/// Spawn the challenge-nonce sweep loop.
#[must_use]
pub fn spawn_challenge_sweep(
    challenges: Arc<Mutex<Challenges>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(CHALLENGE_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let now = now_unix_secs();
                    let mut c = match challenges.lock() {
                        Ok(g) => g,
                        Err(_) => continue,
                    };
                    let _ = c.sweep(now);
                }
            }
        }
    })
}

/// Spawn the metrics tick. Logs aggregate counters at info; does
/// not export anywhere outside the local process.
#[must_use]
pub fn spawn_metrics_tick(
    store: Arc<Store>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(METRICS_INTERVAL);
        // Skip the first immediate tick.
        tick.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    if let Ok(bytes) = store.storage_bytes() {
                        info!(storage_bytes = bytes, "mailbox metrics");
                    }
                }
            }
        }
    })
}

fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_token_stops_each_task_promptly() {
        let store = Arc::new(Store::in_memory().unwrap());
        let chal = Arc::new(Mutex::new(Challenges::default()));
        let cancel = CancellationToken::new();

        let h1 = spawn_expire_sweep(store.clone(), cancel.clone());
        let h2 = spawn_challenge_sweep(chal.clone(), cancel.clone());
        let h3 = spawn_metrics_tick(store, cancel.clone());

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            h1.await.unwrap();
            h2.await.unwrap();
            h3.await.unwrap();
        })
        .await
        .expect("background tasks should stop within 2s");
    }
}
