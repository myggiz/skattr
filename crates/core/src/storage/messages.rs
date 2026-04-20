// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Message history repository.
//!
//! Phase 0.D covers insert + recent-by-group. FTS5 full-text search
//! lands in Phase 1 when the daemon actually holds enough messages for
//! search to matter.

use crate::envelope::Envelope;
use crate::error::{CoreError, Result};
use crate::storage::Pool;

/// A stored message row.
#[derive(Debug, Clone)]
pub(crate) struct StoredMessage {
    pub id: i64,
    pub group_id: Vec<u8>,
    pub sender: Vec<u8>,
    pub kind: String,
    pub body_blob: Option<Vec<u8>>,
    pub ts: i64,
    pub delivered_at: Option<i64>,
}

pub(crate) struct MessageRepo<'p> {
    pool: &'p Pool,
}

impl<'p> MessageRepo<'p> {
    pub(crate) fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a message and return its rowid.
    pub(crate) fn insert(
        &self,
        group_id: &[u8],
        sender: &[u8],
        envelope: &Envelope,
    ) -> Result<i64> {
        let body = envelope.encode()?;
        let kind = match &envelope.kind {
            crate::envelope::Kind::Text { .. } => "text",
            crate::envelope::Kind::File { .. } => "file",
            crate::envelope::Kind::Reaction { .. } => "reaction",
            crate::envelope::Kind::Edit { .. } => "edit",
            crate::envelope::Kind::Delete { .. } => "delete",
            crate::envelope::Kind::Typing => "typing",
        };
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages (group_id, sender, kind, body_blob, ts) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![group_id, sender, kind, body, envelope.ts],
            )
            .map_err(|e| CoreError::Storage(format!("insert message: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Most-recent-first list of messages in a group.
    pub(crate) fn recent(&self, group_id: &[u8], limit: usize) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY ts DESC LIMIT ?2",
                )
                .map_err(|e| CoreError::Storage(format!("prepare recent: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![group_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| {
                        Ok(StoredMessage {
                            id: r.get(0)?,
                            group_id: r.get(1)?,
                            sender: r.get(2)?,
                            kind: r.get(3)?,
                            body_blob: r.get(4)?,
                            ts: r.get(5)?,
                            delivered_at: r.get(6)?,
                        })
                    },
                )
                .map_err(|e| CoreError::Storage(format!("query recent: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect recent: {e}")))
        })
    }

    /// Mark a message delivered. Used by the ACK path.
    pub(crate) fn mark_delivered(&self, id: i64, delivered_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE messages SET delivered_at = ?1 WHERE id = ?2",
                rusqlite::params![delivered_at, id],
            )
            .map_err(|e| CoreError::Storage(format!("mark delivered: {e}")))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Kind, MessageId};

    fn sample_envelope(text: &str) -> Envelope {
        Envelope {
            v: 1,
            id: MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: Kind::Text {
                body: text.to_string(),
            },
        }
    }

    #[test]
    fn insert_returns_rowid_and_round_trips() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xAA; 32];
        let sender = [0x42; 32];
        let env = sample_envelope("hello");

        let id = repo.insert(&gid, &sender, &env).unwrap();
        assert!(id > 0);

        let all = repo.recent(&gid, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].kind, "text");
        assert_eq!(all[0].ts, env.ts);
        // Decode the body_blob back into an Envelope.
        let decoded = Envelope::decode(all[0].body_blob.as_ref().unwrap()).unwrap();
        assert!(matches!(decoded.kind, Kind::Text { body } if body == "hello"));
    }

    #[test]
    fn recent_orders_newest_first_and_limits() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xBB; 32];
        for i in 0..5 {
            let mut env = sample_envelope(&format!("msg-{i}"));
            env.ts = 100 + i as i64;
            repo.insert(&gid, &[0u8; 32], &env).unwrap();
        }
        let three = repo.recent(&gid, 3).unwrap();
        assert_eq!(three.len(), 3);
        assert_eq!(three[0].ts, 104);
        assert_eq!(three[1].ts, 103);
        assert_eq!(three[2].ts, 102);
    }

    #[test]
    fn recent_scoped_to_group_id() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let g1 = [0x11; 32];
        let g2 = [0x22; 32];
        repo.insert(&g1, &[0u8; 32], &sample_envelope("g1")).unwrap();
        repo.insert(&g2, &[0u8; 32], &sample_envelope("g2")).unwrap();
        assert_eq!(repo.recent(&g1, 10).unwrap().len(), 1);
        assert_eq!(repo.recent(&g2, 10).unwrap().len(), 1);
    }

    #[test]
    fn mark_delivered_sets_timestamp() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let id = repo
            .insert(&[0x33; 32], &[0u8; 32], &sample_envelope("x"))
            .unwrap();
        repo.mark_delivered(id, 9999).unwrap();
        let rows = repo.recent(&[0x33; 32], 10).unwrap();
        assert_eq!(rows[0].delivered_at, Some(9999));
    }
}
