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

/// Convert a free-form user query into an FTS5 MATCH expression using
/// the tokenize-and-AND strategy: split on whitespace, wrap each token
/// in FTS5-escaped double quotes (FTS5 doubles internal `"` to `""`),
/// join with ` AND `. Returns `None` if the query is empty or
/// whitespace-only — callers should short-circuit to an empty result
/// without hitting the FTS5 engine.
pub(super) fn fts5_tokenize_and_and(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

/// A stored message row.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub group_id: Vec<u8>,
    pub sender: Vec<u8>,
    pub kind: String,
    pub body_blob: Option<Vec<u8>>,
    pub ts: i64,
    pub delivered_at: Option<i64>,
}

/// All fields required to persist a single message row.
pub struct InsertParams<'a> {
    /// MLS group id this message belongs to.
    pub group_id: &'a [u8],
    /// Sender Ed25519 public key bytes.
    pub sender: &'a [u8],
    /// The decoded application envelope (already MLS-decrypted on the
    /// receiver side, or the local payload on the sender side).
    pub envelope: &'a Envelope,
    /// MLS group epoch at the time the row is persisted. For the
    /// receiver, captured post-decrypt; for the sender, post-encrypt.
    pub mls_generation: u64,
    /// Local clock at the moment the daemon persisted the row.
    pub ts_daemon_recv: i64,
}

/// Message history CRUD operations.
pub struct MessageRepo<'p> {
    pool: &'p Pool,
}

/// One ranked hit returned by [`MessageRepo::search`].
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The full stored message row.
    pub message: StoredMessage,
    /// SQLite FTS5 BM25 score. Lower is better. Always populated; ordering is independent.
    pub bm25: f64,
    /// FTS5 `snippet()` output with delimiter markers and 32-token window.
    pub snippet: String,
}

impl<'p> MessageRepo<'p> {
    /// Construct a new `MessageRepo` backed by `pool`.
    pub fn new(pool: &'p Pool) -> Self {
        Self { pool }
    }

    /// Insert a message and return its rowid. Populates `body_text` for
    /// text-kind envelopes (NULL otherwise), letting the FTS5 triggers
    /// index the row automatically.
    pub fn insert(&self, p: InsertParams<'_>) -> Result<i64> {
        let body = p.envelope.encode()?;
        let kind = match &p.envelope.kind {
            crate::envelope::Kind::Text { .. } => "text",
            crate::envelope::Kind::File { .. } => "file",
            crate::envelope::Kind::Reaction { .. } => "reaction",
            crate::envelope::Kind::Edit { .. } => "edit",
            crate::envelope::Kind::Delete { .. } => "delete",
            crate::envelope::Kind::Typing => "typing",
        };
        let body_text: Option<&str> = match &p.envelope.kind {
            crate::envelope::Kind::Text { body } => Some(body.as_str()),
            _ => None,
        };
        let mls_gen_signed = i64::try_from(p.mls_generation).unwrap_or(i64::MAX);
        self.pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages \
                     (group_id, sender, kind, body_blob, body_text, ts, \
                      mls_generation, ts_daemon_recv) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    p.group_id,
                    p.sender,
                    kind,
                    body,
                    body_text,
                    p.envelope.ts,
                    mls_gen_signed,
                    p.ts_daemon_recv,
                ],
            )
            .map_err(|e| CoreError::Storage(format!("insert message: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Most-recent-first list of messages in a group.
    ///
    /// Ordering is `(mls_generation DESC, id DESC)`. MLS generation is
    /// the authoritative protocol-level epoch (CLAUDE.md: "Authoritative
    /// ordering comes from MLS generation numbers, not `Envelope.ts`");
    /// row `id` DESC is the deterministic tie-breaker among rows
    /// persisted in the same generation. Sender-claimed timestamps are
    /// display-only and never feed into ordering.
    pub fn recent(&self, group_id: &[u8], limit: usize) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY mls_generation DESC, id DESC LIMIT ?2",
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

    /// Full-text search over text-kind message bodies.
    ///
    /// `query` is run through [`fts5_tokenize_and_and`]; whitespace-only
    /// queries return `Ok(vec![])` without hitting FTS5. `group_id =
    /// Some(g)` scopes results to that group.
    ///
    /// Default ordering is BM25 ascending (best first). `newest_first =
    /// true` sorts by `messages.id DESC` regardless of relevance.
    pub fn search(
        &self,
        query: &str,
        group_id: Option<&[u8]>,
        limit: usize,
        offset: usize,
        newest_first: bool,
    ) -> Result<Vec<SearchHit>> {
        let Some(match_expr) = fts5_tokenize_and_and(query) else {
            return Ok(Vec::new());
        };

        let order_clause = if newest_first {
            "messages.id DESC"
        } else {
            "bm25(messages_fts) ASC, messages.id DESC"
        };
        let group_filter = if group_id.is_some() {
            " AND messages.group_id = ?2"
        } else {
            ""
        };
        let limit_offset_first_param = if group_id.is_some() { 3 } else { 2 };

        let sql = format!(
            "SELECT messages.id, messages.group_id, messages.sender, messages.kind, \
                    messages.body_blob, messages.ts, messages.delivered_at, \
                    bm25(messages_fts) AS rank, \
                    snippet(messages_fts, 0, char(2), char(3), '...', 32) AS snippet \
             FROM messages_fts \
             JOIN messages ON messages.id = messages_fts.rowid \
             WHERE messages_fts MATCH ?1{group_filter} \
             ORDER BY {order_clause} \
             LIMIT ?{limit_p} OFFSET ?{offset_p}",
            group_filter = group_filter,
            order_clause = order_clause,
            limit_p = limit_offset_first_param,
            offset_p = limit_offset_first_param + 1,
        );

        self.pool.with(|c| {
            let mut stmt = c
                .prepare(&sql)
                .map_err(|e| CoreError::Storage(format!("prepare search: {e}")))?;

            let limit_i = i64::try_from(limit).unwrap_or(i64::MAX);
            let offset_i = i64::try_from(offset).unwrap_or(0);

            let map_row = |r: &rusqlite::Row<'_>| {
                Ok(SearchHit {
                    message: StoredMessage {
                        id: r.get(0)?,
                        group_id: r.get(1)?,
                        sender: r.get(2)?,
                        kind: r.get(3)?,
                        body_blob: r.get(4)?,
                        ts: r.get(5)?,
                        delivered_at: r.get(6)?,
                    },
                    bm25: r.get::<_, f64>(7).unwrap_or(0.0),
                    snippet: r.get::<_, String>(8).unwrap_or_default(),
                })
            };

            let rows = if let Some(gid) = group_id {
                stmt.query_map(
                    rusqlite::params![match_expr, gid, limit_i, offset_i],
                    map_row,
                )
            } else {
                stmt.query_map(rusqlite::params![match_expr, limit_i, offset_i], map_row)
            }
            .map_err(|e| CoreError::Storage(format!("query search: {e}")))?;

            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect search: {e}")))
        })
    }

    /// Count of messages in `group_id` whose `id` is greater than the
    /// `read_state` cursor. Absent cursor → all rows count as unread.
    pub fn unread_count(&self, group_id: &[u8]) -> Result<u64> {
        self.pool.with(|c| {
            let cursor: Option<i64> = match c.query_row(
                "SELECT last_read_message_id FROM read_state WHERE group_id = ?1",
                rusqlite::params![group_id],
                |r| r.get::<_, i64>(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(CoreError::Storage(format!("unread_count cursor: {e}"))),
            };

            let n: i64 = match cursor {
                Some(cur) => c
                    .query_row(
                        "SELECT COUNT(*) FROM messages \
                         WHERE group_id = ?1 AND id > ?2",
                        rusqlite::params![group_id, cur],
                        |r| r.get(0),
                    )
                    .map_err(|e| CoreError::Storage(format!("unread_count: {e}")))?,
                None => c
                    .query_row(
                        "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                        rusqlite::params![group_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| CoreError::Storage(format!("unread_count: {e}")))?,
            };
            Ok(u64::try_from(n).unwrap_or(0))
        })
    }

    /// Advance the read cursor for `group_id` to `up_to_message_id`.
    /// Idempotent. Caller picks `updated_at` (typically `now() seconds`).
    pub fn mark_read(&self, group_id: &[u8], up_to_message_id: i64) -> Result<()> {
        crate::storage::ReadStateRepo::new(self.pool).set(
            group_id,
            up_to_message_id,
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            )
            .unwrap_or(0),
        )
    }

    /// One page of messages in `group_id`, ordered ascending by `id`
    /// (oldest-first). `after_id = None` starts from the beginning;
    /// `after_id = Some(n)` returns rows with `id > n`. Caller loops
    /// until the returned vec is shorter than `limit`.
    pub fn export_page(
        &self,
        group_id: &[u8],
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at \
                     FROM messages \
                     WHERE group_id = ?1 AND id > ?2 \
                     ORDER BY id ASC \
                     LIMIT ?3",
                )
                .map_err(|e| CoreError::Storage(format!("prepare export_page: {e}")))?;
            let rows = stmt
                .query_map(
                    rusqlite::params![
                        group_id,
                        after_id.unwrap_or(0),
                        i64::try_from(limit).unwrap_or(i64::MAX),
                    ],
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
                .map_err(|e| CoreError::Storage(format!("query export_page: {e}")))?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| CoreError::Storage(format!("collect export_page: {e}")))
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

    /// Delete rows with `ts_daemon_recv < before_ts_recv`. `group_id =
    /// None` prunes globally. Returns the number of rows deleted.
    pub fn prune_before(&self, group_id: Option<&[u8]>, before_ts_recv: i64) -> Result<u64> {
        self.pool.with_mut(|c| {
            let n = if let Some(gid) = group_id {
                c.execute(
                    "DELETE FROM messages \
                     WHERE group_id = ?1 AND ts_daemon_recv < ?2",
                    rusqlite::params![gid, before_ts_recv],
                )
            } else {
                c.execute(
                    "DELETE FROM messages WHERE ts_daemon_recv < ?1",
                    rusqlite::params![before_ts_recv],
                )
            }
            .map_err(|e| CoreError::Storage(format!("prune_before: {e}")))?;
            Ok(u64::try_from(n).unwrap_or(0))
        })
    }

    /// One-shot startup helper: decode CBOR for any text-kind row whose
    /// `body_text` column is NULL (i.e., predates Phase 1.G), populate
    /// it, and let the AU trigger cascade into `messages_fts`. Returns
    /// the number of rows backfilled. Idempotent.
    pub(crate) fn backfill_body_text(&self) -> Result<u64> {
        let candidates: Vec<(i64, Vec<u8>)> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, body_blob FROM messages \
                     WHERE kind = 'text' AND body_text IS NULL",
                )
                .map_err(|e| CoreError::Storage(format!("prepare backfill: {e}")))?;
            let it = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .map_err(|e| CoreError::Storage(format!("query backfill: {e}")))?;
            let v: std::result::Result<Vec<_>, _> = it.collect();
            v.map_err(|e| CoreError::Storage(format!("collect backfill: {e}")))
        })?;

        if candidates.is_empty() {
            return Ok(0);
        }

        let mut updated = 0u64;
        self.pool.with_mut(|c| {
            for (id, blob) in &candidates {
                let env = match crate::envelope::Envelope::decode(blob) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(row_id = id, error = %e,
                            "backfill_body_text: skipping row whose body_blob \
                             failed to decode");
                        continue;
                    }
                };
                if let crate::envelope::Kind::Text { body } = env.kind {
                    c.execute(
                        "UPDATE messages SET body_text = ?1 WHERE id = ?2",
                        rusqlite::params![body, id],
                    )
                    .map_err(|e| CoreError::Storage(format!("backfill UPDATE: {e}")))?;
                    updated += 1;
                }
            }
            Ok(())
        })?;
        Ok(updated)
    }

    /// Keep the `keep` newest rows in `group_id`; delete the rest.
    /// Returns the number of rows deleted.
    pub fn prune_keep_last(&self, group_id: &[u8], keep: u64) -> Result<u64> {
        let keep_i = i64::try_from(keep).unwrap_or(i64::MAX);
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "DELETE FROM messages \
                     WHERE group_id = ?1 \
                       AND id <= COALESCE( \
                           (SELECT id FROM messages \
                            WHERE group_id = ?1 \
                            ORDER BY id DESC \
                            LIMIT 1 OFFSET ?2), \
                           -1 \
                       )",
                    rusqlite::params![group_id, keep_i],
                )
                .map_err(|e| CoreError::Storage(format!("prune_keep_last: {e}")))?;
            Ok(u64::try_from(n).unwrap_or(0))
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

        let id = repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &sender,
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: env.ts,
            })
            .unwrap();
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
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: env.ts,
            })
            .unwrap();
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
        let env_g1 = sample_envelope("g1");
        let env_g2 = sample_envelope("g2");
        repo.insert(InsertParams {
            group_id: &g1,
            sender: &[0u8; 32],
            envelope: &env_g1,
            mls_generation: 0,
            ts_daemon_recv: env_g1.ts,
        })
        .unwrap();
        repo.insert(InsertParams {
            group_id: &g2,
            sender: &[0u8; 32],
            envelope: &env_g2,
            mls_generation: 0,
            ts_daemon_recv: env_g2.ts,
        })
        .unwrap();
        assert_eq!(repo.recent(&g1, 10).unwrap().len(), 1);
        assert_eq!(repo.recent(&g2, 10).unwrap().len(), 1);
    }

    #[test]
    fn mark_delivered_sets_timestamp() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let env = sample_envelope("x");
        let id = repo
            .insert(InsertParams {
                group_id: &[0x33; 32],
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: env.ts,
            })
            .unwrap();
        repo.mark_delivered(id, 9999).unwrap();
        let rows = repo.recent(&[0x33; 32], 10).unwrap();
        assert_eq!(rows[0].delivered_at, Some(9999));
    }

    #[test]
    fn recent_orders_by_id_desc_not_by_ts() {
        // CLAUDE.md: authoritative ordering is NOT ts-based. The repo
        // must return rows with the newest *inserted* first, independent
        // of the sender-claimed `ts` field (which can be backdated).
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = vec![0x42u8; 32];
        let sender = [0x01u8; 32];

        // Insert messages with deliberately non-monotonic `ts`:
        // row1 ts=3000, row2 ts=1000, row3 ts=2000.
        // Expected order from recent(): row3, row2, row1 (id DESC).
        let e1 = Envelope {
            v: 1,
            id: MessageId([1; 16]),
            ts: 3000,
            reply_to: None,
            kind: Kind::Text {
                body: "first".into(),
            },
        };
        let e2 = Envelope {
            v: 1,
            id: MessageId([2; 16]),
            ts: 1000,
            reply_to: None,
            kind: Kind::Text {
                body: "second".into(),
            },
        };
        let e3 = Envelope {
            v: 1,
            id: MessageId([3; 16]),
            ts: 2000,
            reply_to: None,
            kind: Kind::Text {
                body: "third".into(),
            },
        };

        repo.insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &e1,
            mls_generation: 0,
            ts_daemon_recv: e1.ts,
        })
        .unwrap();
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &e2,
            mls_generation: 0,
            ts_daemon_recv: e2.ts,
        })
        .unwrap();
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &e3,
            mls_generation: 0,
            ts_daemon_recv: e3.ts,
        })
        .unwrap();

        let rows = repo.recent(&gid, 10).unwrap();
        // id DESC -> e3 (id=3), e2 (id=2), e1 (id=1).
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].ts, 2000, "row 0 must be last-inserted, not max ts");
        assert_eq!(rows[1].ts, 1000);
        assert_eq!(rows[2].ts, 3000);
    }

    #[test]
    fn fts5_tokenize_and_and_single_token() {
        assert_eq!(
            super::fts5_tokenize_and_and("arti"),
            Some("\"arti\"".to_string())
        );
    }

    #[test]
    fn fts5_tokenize_and_and_multi_token() {
        assert_eq!(
            super::fts5_tokenize_and_and("arti tor"),
            Some("\"arti\" AND \"tor\"".to_string())
        );
    }

    #[test]
    fn fts5_tokenize_and_and_escapes_internal_quotes() {
        // FTS5 escapes " by doubling it. The token `"hi"` (4 chars) becomes
        // `""hi""` after doubling, then wrapped in outer quotes -> `"""hi"""`
        // (3 leading + hi + 3 trailing = 8 chars).
        assert_eq!(
            super::fts5_tokenize_and_and(r#"she said "hi""#),
            Some("\"she\" AND \"said\" AND \"\"\"hi\"\"\"".to_string())
        );
    }

    #[test]
    fn fts5_tokenize_and_and_empty_returns_none() {
        assert_eq!(super::fts5_tokenize_and_and(""), None);
    }

    #[test]
    fn fts5_tokenize_and_and_whitespace_only_returns_none() {
        assert_eq!(super::fts5_tokenize_and_and("   \t\n  "), None);
    }

    #[test]
    fn insert_populates_body_text_for_text_kind_and_fts_indexes_it() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xDD; 32];
        let env = sample_envelope("hello full text search");

        let id = repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &[0x42; 32],
                envelope: &env,
                mls_generation: 7,
                ts_daemon_recv: 1_700_000_500,
            })
            .unwrap();
        assert!(id > 0);

        // body_text column populated
        let body_text: Option<String> = pool
            .with(|c| {
                c.query_row(
                    "SELECT body_text FROM messages WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(body_text.as_deref(), Some("hello full text search"));

        // FTS index returns the row
        let fts_hits: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'search'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(fts_hits, 1, "trigger must have indexed the new row");

        // mls_generation + ts_daemon_recv stored
        let (gen, recv): (i64, i64) = pool
            .with(|c| {
                c.query_row(
                    "SELECT mls_generation, ts_daemon_recv FROM messages WHERE id = ?1",
                    rusqlite::params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(gen, 7);
        assert_eq!(recv, 1_700_000_500);
    }

    #[test]
    fn insert_leaves_body_text_null_for_non_text_kind() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xEE; 32];
        let mut env = sample_envelope("ignored");
        env.kind = crate::envelope::Kind::Typing;

        let id = repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &[0x42; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: 1_700_000_000,
            })
            .unwrap();

        let body_text: Option<String> = pool
            .with(|c| {
                c.query_row(
                    "SELECT body_text FROM messages WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(body_text, None, "non-text kinds must leave body_text NULL");
    }

    fn seed_three_text(pool: &Pool, gid: &[u8; 32]) {
        let repo = MessageRepo::new(pool);
        for (i, body) in ["alpha bravo", "bravo charlie", "delta echo"]
            .iter()
            .enumerate()
        {
            let mut env = sample_envelope(body);
            env.ts = 100 + i as i64;
            repo.insert(InsertParams {
                group_id: gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: u64::try_from(i).unwrap(),
                ts_daemon_recv: 100 + i as i64,
            })
            .unwrap();
        }
    }

    #[test]
    fn search_no_match_returns_empty() {
        let pool = Pool::in_memory();
        seed_three_text(&pool, &[0x10; 32]);
        let hits = MessageRepo::new(&pool)
            .search("zzz", None, 10, 0, false)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_single_token_finds_one_or_more() {
        let pool = Pool::in_memory();
        seed_three_text(&pool, &[0x11; 32]);
        let hits = MessageRepo::new(&pool)
            .search("delta", None, 10, 0, false)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("delta"));
    }

    #[test]
    fn search_multi_token_ands() {
        let pool = Pool::in_memory();
        seed_three_text(&pool, &[0x12; 32]);
        let hits = MessageRepo::new(&pool)
            .search("alpha bravo", None, 10, 0, false)
            .unwrap();
        assert_eq!(hits.len(), 1, "only the row with both tokens should match");
    }

    #[test]
    fn search_empty_query_short_circuits() {
        let pool = Pool::in_memory();
        seed_three_text(&pool, &[0x13; 32]);
        let hits = MessageRepo::new(&pool)
            .search("   ", None, 10, 0, false)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_scoped_to_group_id() {
        let pool = Pool::in_memory();
        let g1 = [0x14; 32];
        let g2 = [0x15; 32];
        seed_three_text(&pool, &g1);
        seed_three_text(&pool, &g2);
        let global = MessageRepo::new(&pool)
            .search("bravo", None, 10, 0, false)
            .unwrap();
        let scoped = MessageRepo::new(&pool)
            .search("bravo", Some(&g1), 10, 0, false)
            .unwrap();
        assert_eq!(global.len(), 4, "two groups × two matches each");
        assert_eq!(scoped.len(), 2);
    }

    #[test]
    fn search_newest_first_orders_by_id_desc() {
        let pool = Pool::in_memory();
        seed_three_text(&pool, &[0x16; 32]);
        let hits = MessageRepo::new(&pool)
            .search("bravo", None, 10, 0, true)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].message.id > hits[1].message.id);
    }

    #[test]
    fn unread_count_returns_total_when_no_cursor() {
        let pool = Pool::in_memory();
        let gid = [0x20; 32];
        seed_three_text(&pool, &gid);
        let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
        assert_eq!(n, 3, "no cursor → all rows are unread");
    }

    #[test]
    fn unread_count_returns_zero_after_cursor_passes_all() {
        use crate::storage::ReadStateRepo;
        let pool = Pool::in_memory();
        let gid = [0x21; 32];
        seed_three_text(&pool, &gid);
        let last_id: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT MAX(id) FROM messages WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        ReadStateRepo::new(&pool)
            .set(&gid, last_id, 1_700_000_000)
            .unwrap();
        let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unread_count_returns_partial_after_cursor_in_middle() {
        use crate::storage::ReadStateRepo;
        let pool = Pool::in_memory();
        let gid = [0x22; 32];
        seed_three_text(&pool, &gid);
        let mid_id: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT id FROM messages WHERE group_id = ?1 \
                     ORDER BY id ASC LIMIT 1 OFFSET 1",
                    rusqlite::params![&gid[..]],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        ReadStateRepo::new(&pool)
            .set(&gid, mid_id, 1_700_000_000)
            .unwrap();
        let n = MessageRepo::new(&pool).unread_count(&gid).unwrap();
        assert_eq!(n, 1, "1 of 3 rows has id > cursor");
    }

    #[test]
    fn recent_by_contact_orders_by_mls_generation_then_id() {
        let pool = Pool::in_memory();
        let gid = [0x23; 32];
        let repo = MessageRepo::new(&pool);
        // Insert with mixed mls_generation values.
        for (gen, body, ts) in [
            (2, "first-but-newer-gen", 100),
            (5, "third-yet-older-gen", 102),
            (3, "second", 101),
        ] {
            let mut env = sample_envelope(body);
            env.ts = ts;
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: gen,
                ts_daemon_recv: ts,
            })
            .unwrap();
        }
        let rows: Vec<i64> = pool
            .with(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT id FROM messages WHERE group_id = ?1 \
                     ORDER BY mls_generation DESC, id DESC",
                    )
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                let it = stmt
                    .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                let v: std::result::Result<Vec<_>, _> = it.collect();
                v.map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        // Highest mls_generation first → "third-yet-older-gen" (gen=5),
        // then "second" (gen=3), then "first-but-newer-gen" (gen=2).
        assert_eq!(rows.len(), 3);
        let bodies: Vec<String> = pool
            .with(|c| {
                let mut stmt = c
                    .prepare("SELECT body_text FROM messages WHERE id = ?1")
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for id in &rows {
                    let body: String = stmt
                        .query_row(rusqlite::params![id], |r| r.get(0))
                        .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                    out.push(body);
                }
                Ok(out)
            })
            .unwrap();
        assert_eq!(
            bodies,
            vec![
                "third-yet-older-gen".to_string(),
                "second".to_string(),
                "first-but-newer-gen".to_string(),
            ]
        );

        // Repo's recent() must agree with the upgraded SQL: same ordering.
        let recent = repo.recent(&gid, 10).unwrap();
        let recent_ids: Vec<i64> = recent.iter().map(|r| r.id).collect();
        assert_eq!(recent_ids, rows);
    }

    #[test]
    fn mark_read_advances_cursor_idempotent() {
        let pool = Pool::in_memory();
        let gid = [0x30; 32];
        seed_three_text(&pool, &gid);
        let repo = MessageRepo::new(&pool);

        repo.mark_read(&gid, 42).unwrap();
        repo.mark_read(&gid, 42).unwrap(); // idempotent overwrite

        use crate::storage::ReadStateRepo;
        assert_eq!(ReadStateRepo::new(&pool).get(&gid).unwrap(), Some(42));
    }

    #[test]
    fn mark_read_updates_existing_cursor() {
        let pool = Pool::in_memory();
        let gid = [0x31; 32];
        seed_three_text(&pool, &gid);
        let repo = MessageRepo::new(&pool);

        repo.mark_read(&gid, 10).unwrap();
        repo.mark_read(&gid, 99).unwrap();

        use crate::storage::ReadStateRepo;
        assert_eq!(ReadStateRepo::new(&pool).get(&gid).unwrap(), Some(99));
    }

    #[test]
    fn export_page_yields_oldest_first_full_page() {
        let pool = Pool::in_memory();
        let gid = [0x40; 32];
        let repo = MessageRepo::new(&pool);
        for i in 0..5i64 {
            let mut env = sample_envelope(&format!("msg-{i}"));
            env.ts = 1000 + i;
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: 1000 + i,
            })
            .unwrap();
        }
        let page = repo.export_page(&gid, None, 10).unwrap();
        assert_eq!(page.len(), 5);
        assert!(page[0].id < page[4].id, "oldest first");
    }

    #[test]
    fn export_page_paginates_via_after_id() {
        let pool = Pool::in_memory();
        let gid = [0x41; 32];
        let repo = MessageRepo::new(&pool);
        for i in 0..7i64 {
            let mut env = sample_envelope(&format!("p-{i}"));
            env.ts = 1000 + i;
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: 1000 + i,
            })
            .unwrap();
        }
        let page1 = repo.export_page(&gid, None, 3).unwrap();
        assert_eq!(page1.len(), 3);
        let page2 = repo
            .export_page(&gid, Some(page1.last().unwrap().id), 3)
            .unwrap();
        assert_eq!(page2.len(), 3);
        let page3 = repo
            .export_page(&gid, Some(page2.last().unwrap().id), 3)
            .unwrap();
        assert_eq!(page3.len(), 1);
        assert!(page1.last().unwrap().id < page2.first().unwrap().id);
        assert!(page2.last().unwrap().id < page3.first().unwrap().id);
    }

    #[test]
    fn prune_before_deletes_old_rows_and_cascades_to_fts() {
        let pool = Pool::in_memory();
        let gid = [0x50; 32];
        let repo = MessageRepo::new(&pool);
        for i in 0..6i64 {
            let mut env = sample_envelope(&format!("retain-or-prune-{i}"));
            env.ts = 1000;
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: 0,
                ts_daemon_recv: i * 100, // 0..500 in steps of 100
            })
            .unwrap();
        }

        let deleted = repo.prune_before(Some(&gid), 250).unwrap();
        assert_eq!(deleted, 3, "rows with ts_daemon_recv 0/100/200 must go");

        let remaining: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(remaining, 3);

        let fts_rows: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts \
                     MATCH 'retain'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(fts_rows, 3, "ad trigger must cascade FTS deletes");
    }

    #[test]
    fn prune_before_global_when_group_is_none() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        for gid in [&[0x60u8; 32][..], &[0x61u8; 32][..]] {
            for i in 0..3i64 {
                let mut env = sample_envelope(&format!("g-{i}"));
                env.ts = 1000;
                repo.insert(InsertParams {
                    group_id: gid,
                    sender: &[0u8; 32],
                    envelope: &env,
                    mls_generation: 0,
                    ts_daemon_recv: i * 100,
                })
                .unwrap();
            }
        }
        let deleted = repo.prune_before(None, 150).unwrap();
        assert_eq!(deleted, 4, "two rows from each of two groups (ts<150)");
    }

    #[test]
    fn prune_keep_last_keeps_most_recent() {
        let pool = Pool::in_memory();
        let gid = [0x70; 32];
        let repo = MessageRepo::new(&pool);
        for i in 0..10i64 {
            let mut env = sample_envelope(&format!("k-{i}"));
            env.ts = 1000;
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &env,
                mls_generation: u64::try_from(i).unwrap(),
                ts_daemon_recv: i,
            })
            .unwrap();
        }
        let deleted = repo.prune_keep_last(&gid, 3).unwrap();
        assert_eq!(deleted, 7);
        let remaining_ids: Vec<i64> = pool
            .with(|c| {
                let mut stmt = c
                    .prepare("SELECT id FROM messages WHERE group_id = ?1 ORDER BY id DESC")
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                let it = stmt
                    .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                    .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
                let v: std::result::Result<Vec<_>, _> = it.collect();
                v.map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(remaining_ids.len(), 3, "exactly 3 rows survive");
        let max = remaining_ids.iter().copied().max().unwrap();
        let min = remaining_ids.iter().copied().min().unwrap();
        assert_eq!(max - min, 2, "the surviving 3 are consecutive at the top");
    }

    #[test]
    fn backfill_body_text_decodes_legacy_text_rows_and_indexes_fts() {
        let pool = Pool::in_memory();
        let gid = [0x80u8; 32];

        // Insert a row directly with body_text NULL (simulating a pre-1.G row).
        let env = sample_envelope("legacy hello world");
        let blob = env.encode().unwrap();
        let sender = [0u8; 32];
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages \
                     (group_id, sender, kind, body_blob, body_text, ts, \
                      mls_generation, ts_daemon_recv) \
                 VALUES (?1, ?2, 'text', ?3, NULL, ?4, 0, 0)",
                rusqlite::params![&gid[..], &sender[..], blob, env.ts],
            )
            .map_err(|e| crate::error::CoreError::Storage(e.to_string()))?;
            Ok(())
        })
        .unwrap();

        // Sanity: FTS index is empty before backfill.
        let pre: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts \
                     WHERE messages_fts MATCH 'legacy'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(pre, 0);

        let n = MessageRepo::new(&pool).backfill_body_text().unwrap();
        assert_eq!(n, 1);

        // Backfilled row's body_text populated; FTS index now finds it.
        let post: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts \
                     WHERE messages_fts MATCH 'legacy'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| crate::error::CoreError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(post, 1, "au trigger must have indexed the row");
    }

    #[test]
    fn backfill_body_text_is_idempotent() {
        let pool = Pool::in_memory();
        let gid = [0x81u8; 32];
        let repo = MessageRepo::new(&pool);
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &sample_envelope("already populated"),
            mls_generation: 0,
            ts_daemon_recv: 0,
        })
        .unwrap();
        // body_text already populated by insert; backfill must do nothing.
        let n = repo.backfill_body_text().unwrap();
        assert_eq!(n, 0);
    }
}
