// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Persisted send queue with exponential-backoff retry.
//!
//! Rows live in the `outbox` table. Each row represents one message to
//! one recipient (group messages fan out to N rows). A background task
//! scans for `next_retry_at <= now` and attempts delivery; on ACK the
//! row is removed. See design §1.5 and the Phase 1 delivery workstream.

use crate::envelope::MessageId;
use crate::error::Result;
use crate::identity::PublicKey;

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
    /// Unix timestamp when we may next attempt delivery.
    pub next_retry_at: i64,
}

/// Persisted outbox interface.
#[derive(Debug)]
pub struct Outbox {
    _private: (),
}

impl Outbox {
    /// Enqueue a message for a recipient.
    pub fn enqueue(&self, _entry: OutboxEntry) -> Result<()> {
        todo!("INSERT into outbox")
    }

    /// Pop (up to `max`) entries that are due for a retry.
    pub fn due(&self, _max: usize) -> Result<Vec<OutboxEntry>> {
        todo!("SELECT … WHERE next_retry_at <= now ORDER BY next_retry_at LIMIT max")
    }

    /// Mark an entry delivered and remove it.
    pub fn ack(&self, _message_id: MessageId) -> Result<()> {
        todo!("DELETE FROM outbox WHERE message_id = ?")
    }

    /// Reschedule an entry after a failed attempt.
    pub fn reschedule(&self, _id: i64) -> Result<()> {
        todo!("UPDATE outbox SET attempts = attempts+1, next_retry_at = … WHERE id = ?")
    }
}
