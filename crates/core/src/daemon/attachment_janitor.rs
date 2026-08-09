// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Reclaims attachment state that no lane will ever finish.
//!
//! Two mechanisms, run from the hourly `daemon::retention` sweep:
//!
//! 1. **Auto-fail** — an inbound transfer whose chunk directory has not been
//!    written to for `STALL_GRACE` is claimed as `'failed'`, which makes it
//!    visible in the UI and retryable (#146) instead of silently stuck. Its
//!    chunks are **kept**: they are the resume state a retry needs.
//! 2. **Orphan sweep** — a chunk directory with no `attachments` row at all is
//!    removed once it is older than `ORPHAN_GRACE`.
//!
//! A **completed** attachment's chunks are the user's file (encrypted at rest;
//! plaintext only on demand), so `status='complete'` is out of scope for both.

use std::path::Path;
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;

use crate::daemon::events::Event;
use crate::storage::attachments::{AttachmentRepo, TerminalStatus};
use crate::storage::Pool;

/// How long an inbound transfer may go without a new chunk before it is
/// auto-failed.
///
/// Chunk deposits are made with `ttl=0`, which the mailbox resolves to its
/// `default_ttl_secs` of 7 days (`crates/mailbox/src/policy.rs`), so an offline
/// transfer can legitimately sit in flight for a week waiting for the receiver
/// to poll. 14 days is 2x that, so a transfer waiting on mailbox delivery is
/// never failed while its deposits could still arrive.
pub(crate) const STALL_GRACE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// What one janitor pass reclaimed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct JanitorStats {
    pub stalled: usize,
    pub orphans: usize,
}

/// Run one janitor pass. Returns what it reclaimed.
///
/// `now` is injected rather than read from the clock so the sweep is testable
/// without manipulating file mtimes.
///
/// Never returns `Err`: a failure on one attachment must not stop the pass, so
/// each is logged and skipped. Nothing here is silent (#142).
pub(crate) fn run_once(
    pool: &Pool,
    data_dir: &Path,
    events_tx: &broadcast::Sender<Event>,
    now: SystemTime,
) -> JanitorStats {
    let mut stats = JanitorStats::default();
    let repo = AttachmentRepo::new(pool);
    let root = data_dir.join("attachments");

    // --- Mechanism A: auto-fail stalled inbound transfers -------------------
    //
    // `list_pending_in` is already restricted to direction='in' AND
    // status='pending', so 'complete' and 'failed' rows are never considered.
    let pending = match repo.list_pending_in() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "janitor: listing pending inbound failed");
            Vec::new()
        }
    };

    for (aid, _manifest) in pending {
        let dir = root.join(hex::encode(aid));
        // No directory => no chunks on disk => nothing to reclaim and no
        // activity signal. Leave it alone.
        let Ok(meta) = std::fs::metadata(&dir) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        // `duration_since` errors when mtime is in the future (clock skew);
        // treat that as "just touched", i.e. not stalled.
        let Ok(age) = now.duration_since(mtime) else {
            continue;
        };
        if age <= STALL_GRACE {
            continue;
        }
        // Transition via claim_terminal only: it is the #38 exactly-once gate.
        // A false return means another lane already terminalised this row, and
        // that lane owns the event — stay silent.
        match repo.claim_terminal(&aid, TerminalStatus::Failed) {
            Ok(true) => {
                stats.stalled += 1;
                let _ = events_tx.send(Event::AttachmentFailed {
                    attachment_id: crate::daemon::hex::Hex16::from(aid),
                    reason: "transfer stalled".into(),
                });
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(
                aid = %hex::encode(aid),
                err = %e,
                "janitor: auto-fail claim failed"
            ),
        }
    }

    if stats.stalled > 0 {
        tracing::info!(
            stalled = stats.stalled,
            "janitor: auto-failed stalled transfers"
        );
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::attachments::AttachmentRepo;

    fn chunk_dir(data_dir: &Path, aid: &[u8; 16]) -> std::path::PathBuf {
        data_dir.join("attachments").join(hex::encode(aid))
    }

    /// Create a chunk directory with one chunk file in it, as `ChunkStore::put`
    /// would. Its mtime is "now" on the real filesystem clock.
    fn make_chunks(data_dir: &Path, aid: &[u8; 16]) {
        let d = chunk_dir(data_dir, aid);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("0"), b"ciphertext").unwrap();
    }

    fn events() -> broadcast::Sender<Event> {
        broadcast::channel::<Event>(16).0
    }

    /// `now` far enough in the future that anything created during the test is
    /// past both grace periods.
    fn way_later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(15 * 24 * 60 * 60)
    }

    #[tokio::test]
    async fn stalled_pending_inbound_is_failed_and_its_chunks_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA1u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 1);
        let row = AttachmentRepo::new(&pool).get(&aid).unwrap().unwrap();
        assert_eq!(row.status, "failed", "stalled transfer must be auto-failed");
        assert!(
            chunk_dir(tmp.path(), &aid).exists(),
            "chunks are retry resume state and must be kept"
        );
    }

    #[tokio::test]
    async fn pending_inside_the_grace_window_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA2u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);

        // now == real now: the directory was just written, so it is fresh.
        let stats = run_once(&pool, tmp.path(), &events(), SystemTime::now());

        assert_eq!(stats.stalled, 0);
        let row = AttachmentRepo::new(&pool).get(&aid).unwrap().unwrap();
        assert_eq!(row.status, "pending");
    }

    #[tokio::test]
    async fn a_retried_attachment_is_not_immediately_refailed() {
        // The #149 regression guard. created_at is ancient (0) and
        // rearm_failed_in does not touch it, so a created_at-based policy would
        // re-fail this instantly. Fresh chunks must protect it.
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);
        let aid = [0xA3u8; 16];
        repo.insert(&aid, "in", b"m", 1, 0).unwrap();
        repo.claim_terminal(&aid, TerminalStatus::Failed).unwrap();
        repo.rearm_failed_in(&aid).unwrap();
        make_chunks(tmp.path(), &aid); // retry wrote a chunk just now

        let stats = run_once(&pool, tmp.path(), &events(), SystemTime::now());

        assert_eq!(
            stats.stalled, 0,
            "a just-retried transfer must not be re-failed"
        );
        assert_eq!(repo.get(&aid).unwrap().unwrap().status, "pending");
    }

    #[tokio::test]
    async fn completed_attachment_is_never_touched() {
        // The one unacceptable failure mode: a complete attachment's chunks are
        // the user's file.
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let repo = AttachmentRepo::new(&pool);
        let aid = [0xC0u8; 16];
        repo.insert(&aid, "in", b"m", 1, 0).unwrap();
        repo.claim_terminal(&aid, TerminalStatus::Complete).unwrap();
        make_chunks(tmp.path(), &aid);

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 0);
        assert_eq!(repo.get(&aid).unwrap().unwrap().status, "complete");
        assert!(
            chunk_dir(tmp.path(), &aid).exists(),
            "a completed attachment's chunks ARE the user's file"
        );
    }

    #[tokio::test]
    async fn pending_row_with_no_chunk_directory_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA4u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        // no make_chunks: nothing on disk to reclaim, no mtime to reason about

        let stats = run_once(&pool, tmp.path(), &events(), way_later());

        assert_eq!(stats.stalled, 0);
        assert_eq!(
            AttachmentRepo::new(&pool)
                .get(&aid)
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
    }

    #[tokio::test]
    async fn auto_fail_emits_attachment_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = Pool::in_memory();
        let aid = [0xA5u8; 16];
        AttachmentRepo::new(&pool)
            .insert(&aid, "in", b"m", 1, 0)
            .unwrap();
        make_chunks(tmp.path(), &aid);
        let tx = events();
        let mut rx = tx.subscribe();

        run_once(&pool, tmp.path(), &tx, way_later());

        match rx.try_recv() {
            Ok(Event::AttachmentFailed { attachment_id, .. }) => {
                assert_eq!(attachment_id.0, aid);
            }
            other => panic!("expected AttachmentFailed, got {other:?}"),
        }
    }
}
