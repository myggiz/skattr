// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Persisted send queue with exponential-backoff retry.
//!
//! Thin wrapper over [`crate::storage::outbox::OutboxRepo`] that speaks
//! in `PublicKey`/`MessageId` terms rather than raw byte slices. Rows
//! live in the `outbox` table (see migration 0001 + 0004).

use std::time::Duration;

use crate::delivery::backoff::backoff;
use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::PublicKey;
use crate::storage::outbox::{OutboxRepo, OutboxRow};
use crate::storage::Pool;

/// A pending outbound delivery.
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    /// Row id (rowid in SQLite).
    pub id: i64,
    /// Intended recipient.
    pub target: PublicKey,
    /// Opaque encrypted payload (MLS ciphertext already wrapped).
    pub payload: Vec<u8>,
    /// Application message id, for ACK correlation.
    pub message_id: MessageId,
    /// Retry attempt count (0 on first enqueue).
    pub attempts: u32,
}

/// Borrowed view over the outbox backed by a `Pool`.
pub struct Outbox<'p> {
    repo: OutboxRepo<'p>,
}

impl<'p> Outbox<'p> {
    /// Create a new `Outbox` backed by the given `Pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self {
            repo: OutboxRepo::new(pool),
        }
    }

    /// Enqueue a fresh `(target, message_id, payload)` tuple with
    /// `next_retry_at = now`. Returns `Ok(Some(rowid))` on fresh
    /// insert, `Ok(None)` if `(target, message_id)` is already present.
    pub fn enqueue(
        &self,
        target: &PublicKey,
        message_id: MessageId,
        payload: &[u8],
        now: i64,
    ) -> Result<Option<i64>> {
        self.repo.insert(&target.0, &message_id.0, payload, now)
    }

    /// Entries whose `next_retry_at` has passed, up to `max`.
    pub fn due(&self, now: i64, max: usize) -> Result<Vec<OutboxEntry>> {
        let rows = self.repo.due(now, max)?;
        Ok(rows.into_iter().map(row_to_entry).collect())
    }

    /// Delete the `(target, message_id)` row. Returns `true` if a row
    /// was removed.
    pub fn ack(&self, target: &PublicKey, message_id: MessageId) -> Result<bool> {
        self.repo.ack_by_message_id(&target.0, &message_id.0)
    }

    /// Bump `attempts` and set `next_retry_at = now + backoff(attempts_now)`.
    pub fn reschedule(&self, id: i64, attempts_now: u32, now: i64) -> Result<()> {
        let delay = backoff(attempts_now);
        let next_retry_at =
            now.saturating_add(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX));
        self.repo.reschedule(id, next_retry_at)
    }

    /// Convenience: the configured cap used by [`backoff`]. Exposed
    /// for the retry-tick ceiling when logging.
    #[cfg(test)]
    pub(crate) fn backoff_cap() -> Duration {
        crate::delivery::backoff::CAP
    }
}

fn row_to_entry(row: OutboxRow) -> OutboxEntry {
    let mut pk = [0u8; 32];
    if row.target.len() == 32 {
        pk.copy_from_slice(&row.target);
    }
    OutboxEntry {
        id: row.id,
        target: PublicKey(pk),
        payload: row.payload,
        message_id: MessageId(row.message_id),
        attempts: row.attempts,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn pk(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    fn mid(byte: u8) -> MessageId {
        MessageId([byte; 16])
    }

    #[test]
    fn enqueue_is_idempotent_on_target_message_id() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        assert!(ob
            .enqueue(&pk(0xAA), mid(0x01), b"p", 100)
            .unwrap()
            .is_some());
        assert!(ob
            .enqueue(&pk(0xAA), mid(0x01), b"p", 100)
            .unwrap()
            .is_none());
    }

    #[test]
    fn due_returns_entries_with_public_key_and_message_id() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        ob.enqueue(&pk(0xAA), mid(0x01), b"past", 100).unwrap();
        let list = ob.due(999, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target, pk(0xAA));
        assert_eq!(list[0].message_id, mid(0x01));
        assert_eq!(list[0].payload, b"past");
        assert_eq!(list[0].attempts, 0);
    }

    #[test]
    fn ack_removes_exactly_one_row() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        ob.enqueue(&pk(0xAA), mid(0x01), b"p", 100).unwrap();
        assert!(ob.ack(&pk(0xAA), mid(0x01)).unwrap());
        assert!(ob.due(999, 10).unwrap().is_empty());
    }

    #[test]
    fn reschedule_bumps_attempts_and_next_retry() {
        let pool = Pool::in_memory();
        let ob = Outbox::new(&pool);
        let rid = ob
            .enqueue(&pk(0xAA), mid(0x01), b"p", 100)
            .unwrap()
            .unwrap();
        ob.reschedule(rid, 0, 1_000).unwrap();
        // With backoff(0) ∈ [0.75s, 1.25s], next_retry_at ∈ [1750, 2250].
        // Immediately after reschedule, due(now=1_499) should return nothing.
        assert!(ob.due(1_499, 10).unwrap().is_empty());
        let later = ob.due(3_000, 10).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].attempts, 1);
    }
}
