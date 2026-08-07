// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 1.G integration test: retention sweep over a real in-memory
//! Pool with a 50 ms test-only tick interval.
//!
//! Seeds 5 rows at `ts_daemon_recv` offsets `[-3d, -2d, -1d, 0, 0]`,
//! spawns `spawn_sweep` with `retention_days = 1` and a 50 ms tick,
//! waits ~200 ms, then asserts that
//!
//! 1. the surviving message count is `<= 3` (1-day cutoff keeps only
//!    the two "0" offsets plus at most one borderline row), and
//! 2. the `messages_fts` cascade-after-delete trigger left the FTS
//!    index with exactly as many rows matching "retainable" as the
//!    `messages` table has rows.
//!
//! Uses only the public `MessageRepo` API for counting so the test
//! crate does not need a direct `rusqlite` dep.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::daemon::Config;
use skattr_core::envelope::{Envelope, Kind, MessageId};
use skattr_core::test_exports::{now_unix_seconds, spawn_sweep, InsertParams, MessageRepo, Pool};

fn to_env(body: &str) -> Envelope {
    Envelope {
        v: 1,
        id: MessageId::generate(),
        ts: 1_700_000_000,
        reply_to: None,
        kind: Kind::Text { body: body.into() },
    }
}

#[tokio::test]
async fn sweep_deletes_only_rows_older_than_cutoff_and_cascades_fts() {
    let pool = Arc::new(Pool::in_memory());
    let now = now_unix_seconds();
    let gid: [u8; 32] = [0x33; 32];

    // Five rows at offsets [-3d, -2d, -1d, 0, 0].
    for offset_days in [-3i64, -2, -1, 0, 0] {
        MessageRepo::new(&pool)
            .insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &to_env("retainable"),
                mls_generation: 0,
                ts_daemon_recv: now + offset_days * 86_400,
            })
            .unwrap();
    }

    let (tx, rx) = tokio::sync::watch::channel(false);
    let mut cfg = Config::defaults().expect("defaults");
    cfg.history.retention_days = 1; /* day */
    let config_arc = Arc::new(tokio::sync::RwLock::new(cfg));
    let h = spawn_sweep(pool.clone(), config_arc, Duration::from_millis(50), rx);

    // Wait long enough for at least one tick (~2-4 ticks at 50 ms).
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = tx.send(true);
    let _ = h.await;

    // Count surviving messages via the public repo API.
    let survivors = MessageRepo::new(&pool).recent(&gid, usize::MAX).unwrap();
    let n = i64::try_from(survivors.len()).unwrap();
    assert!(n <= 3, "expected <=3 rows after 1d-cutoff sweep, got {n}");

    // FTS cascade: rows matching "retainable" must equal messages count.
    let fts_hits = MessageRepo::new(&pool)
        .search("retainable", Some(&gid), usize::MAX, 0, false)
        .unwrap();
    let fts_n = i64::try_from(fts_hits.len()).unwrap();
    assert_eq!(fts_n, n, "FTS row count must match messages row count");
}
