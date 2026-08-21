// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Message history repository.
//!
//! Covers insert, recent-by-group, and FTS5 full-text search (`search`,
//! `fts5_tokenize_and_and`; the index is created by migration 0006).
//!
//! Ordering differs by query, and never uses `Envelope.ts` (which is
//! display-only): conversation reads order by `mls_generation DESC, id DESC`,
//! while `search` orders by `messages.id DESC` (newest-first) or
//! `bm25(messages_fts) ASC, messages.id DESC` (by relevance).

use crate::envelope::Envelope;
use crate::error::{CoreError, Result};
use crate::storage::{Pool, StorageErrorKind};

/// Map a message string to either a [`StorageErrorKind::FtsSyntax`] or
/// [`StorageErrorKind::Other`] `CoreError::Storage` variant.
///
/// Called only from the FTS5 search path, where sqlite can surface
/// both `fts5: syntax error` and `malformed MATCH` strings.
fn fts_or_other(msg: String) -> CoreError {
    if msg.contains("fts5: syntax error") || msg.contains("malformed MATCH") {
        CoreError::Storage(StorageErrorKind::FtsSyntax(msg))
    } else {
        CoreError::Storage(StorageErrorKind::Other(msg))
    }
}

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
    /// MLS group epoch at persist time. 0 for legacy rows written
    /// before Phase 1.G when the column did not yet exist.
    pub mls_generation: i64,
    /// Local-clock unix seconds at persist time. 0 for legacy rows.
    pub ts_daemon_recv: i64,
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

    /// Insert a message inside the caller's transaction and return its
    /// rowid. Use this when the message row must commit atomically with
    /// other rows (e.g. MLS snapshot + outbox in one transaction).
    ///
    /// Preserves the exact error taxonomy from `insert`:
    /// - `SQLITE_CONSTRAINT_UNIQUE` → `StorageErrorKind::DuplicateMessage`
    /// - Other sqlite errors → `StorageErrorKind::Other("messages INSERT: …")`
    pub(crate) fn insert_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        p: InsertParams<'_>,
    ) -> Result<i64> {
        let body = p.envelope.encode()?;
        let kind = match &p.envelope.kind {
            crate::envelope::Kind::Text { .. } => "text",
            crate::envelope::Kind::File { .. } => "file",
            crate::envelope::Kind::Reaction { .. } => "reaction",
            crate::envelope::Kind::Edit { .. } => "edit",
            crate::envelope::Kind::Delete { .. } => "delete",
            crate::envelope::Kind::Typing => "typing",
            crate::envelope::Kind::ContactCardUpdate { .. } => unreachable!(
                "ContactCardUpdate is intercepted in DaemonInbound; never reaches MessageRepo"
            ),
        };
        let body_text: Option<&str> = match &p.envelope.kind {
            crate::envelope::Kind::Text { body } => Some(body.as_str()),
            _ => None,
        };
        let mls_gen_signed = i64::try_from(p.mls_generation).unwrap_or(i64::MAX);
        let envelope_id = &p.envelope.id.0[..];
        match tx.execute(
            "INSERT INTO messages \
                 (group_id, sender, envelope_id, kind, body_blob, body_text, ts, \
                  mls_generation, ts_daemon_recv) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                p.group_id,
                p.sender,
                envelope_id,
                kind,
                body,
                body_text,
                p.envelope.ts,
                mls_gen_signed,
                p.ts_daemon_recv,
            ],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                return Err(CoreError::Storage(StorageErrorKind::DuplicateMessage));
            }
            Err(e) => {
                return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "messages INSERT: {e}"
                ))));
            }
        }
        Ok(tx.last_insert_rowid())
    }

    /// Insert a message and return its rowid. Populates `body_text` for
    /// text-kind envelopes (NULL otherwise), letting the FTS5 triggers
    /// index the row automatically.
    pub fn insert(&self, p: InsertParams<'_>) -> Result<i64> {
        self.pool.transaction(|tx| self.insert_in_tx(tx, p))
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
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                            mls_generation, ts_daemon_recv \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY mls_generation DESC, id DESC LIMIT ?2",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare recent: {e}")))
                })?;
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
                            mls_generation: r.get(7)?,
                            ts_daemon_recv: r.get(8)?,
                        })
                    },
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query recent: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect recent: {e}")))
            })
        })
    }

    /// Paginate older messages: rows with `id < before_id`.
    /// Ordering matches `recent` — `(mls_generation DESC, id DESC) LIMIT n`.
    /// Cursor row is excluded (strict-less semantics).
    pub fn recent_before(
        &self,
        group_id: &[u8],
        before_id: i64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                            mls_generation, ts_daemon_recv \
                     FROM messages \
                     WHERE group_id = ?1 AND id < ?2 \
                     ORDER BY mls_generation DESC, id DESC LIMIT ?3",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare recent_before: {e}"
                    )))
                })?;
            let rows = stmt
                .query_map(
                    rusqlite::params![
                        group_id,
                        before_id,
                        i64::try_from(limit).unwrap_or(i64::MAX)
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
                            mls_generation: r.get(7)?,
                            ts_daemon_recv: r.get(8)?,
                        })
                    },
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query recent_before: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "collect recent_before: {e}"
                )))
            })
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
                    messages.mls_generation, messages.ts_daemon_recv, \
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
                .map_err(|e| fts_or_other(format!("prepare search: {e}")))?;

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
                        mls_generation: r.get(7)?,
                        ts_daemon_recv: r.get(8)?,
                    },
                    bm25: r.get::<_, f64>(9).unwrap_or(0.0),
                    snippet: r.get::<_, String>(10).unwrap_or_default(),
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
            .map_err(|e| fts_or_other(format!("query search: {e}")))?;

            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| fts_or_other(format!("collect search: {e}")))
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
                Err(e) => {
                    return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                        "unread_count cursor: {e}"
                    ))))
                }
            };

            let n: i64 = match cursor {
                Some(cur) => c
                    .query_row(
                        "SELECT COUNT(*) FROM messages \
                         WHERE group_id = ?1 AND id > ?2",
                        rusqlite::params![group_id, cur],
                        |r| r.get(0),
                    )
                    .map_err(|e| {
                        CoreError::Storage(StorageErrorKind::Other(format!("unread_count: {e}")))
                    })?,
                None => c
                    .query_row(
                        "SELECT COUNT(*) FROM messages WHERE group_id = ?1",
                        rusqlite::params![group_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| {
                        CoreError::Storage(StorageErrorKind::Other(format!("unread_count: {e}")))
                    })?,
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
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                            mls_generation, ts_daemon_recv \
                     FROM messages \
                     WHERE group_id = ?1 AND id > ?2 \
                     ORDER BY id ASC \
                     LIMIT ?3",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare export_page: {e}")))
                })?;
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
                            mls_generation: r.get(7)?,
                            ts_daemon_recv: r.get(8)?,
                        })
                    },
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query export_page: {e}")))
                })?;
            let out: std::result::Result<Vec<_>, _> = rows.collect();
            out.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect export_page: {e}")))
            })
        })
    }

    /// Mark a message delivered, addressed by the 16-byte envelope id the peer
    /// sends in its `Frame::Ack` (#200).
    ///
    /// Returns whether a row was updated. `false` is not an error: an ACK can
    /// arrive for a message this node no longer holds (retention pruning), or
    /// after the row was already marked.
    ///
    /// Scoped to rows **we** sent (`sender = self_pubkey`): a delivery receipt
    /// is only meaningful for an outgoing message, and without that scope an
    /// authenticated peer could ACK the envelope id of a message *it* sent us
    /// and forge a Delivered receipt on an incoming row. `delivered_at IS NULL`
    /// keeps a re-delivered ACK from moving an already-recorded timestamp.
    pub(crate) fn mark_delivered_by_envelope_id(
        &self,
        envelope_id: &[u8; 16],
        self_pubkey: &[u8; 32],
        delivered_at: i64,
    ) -> Result<bool> {
        self.pool.with_mut(|c| {
            let n = c
                .execute(
                    "UPDATE messages SET delivered_at = ?1                      WHERE envelope_id = ?2 AND sender = ?3 AND delivered_at IS NULL",
                    rusqlite::params![delivered_at, &envelope_id[..], &self_pubkey[..]],
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("mark delivered: {e}")))
                })?;
            Ok(n > 0)
        })
    }

    /// Mark a message delivered by row id.
    #[cfg(test)]
    pub(crate) fn mark_delivered(&self, id: i64, delivered_at: i64) -> Result<()> {
        self.pool.with_mut(|c| {
            c.execute(
                "UPDATE messages SET delivered_at = ?1 WHERE id = ?2",
                rusqlite::params![delivered_at, id],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("mark delivered: {e}")))
            })?;
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
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("prune_before: {e}")))
            })?;
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
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prepare backfill: {e}")))
                })?;
            let it = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("query backfill: {e}")))
                })?;
            let v: std::result::Result<Vec<_>, _> = it.collect();
            v.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("collect backfill: {e}")))
            })
        })?;

        if candidates.is_empty() {
            return Ok(0);
        }

        let mut updated = 0u64;
        self.pool.transaction(|tx| {
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
                    tx.execute(
                        "UPDATE messages SET body_text = ?1 WHERE id = ?2",
                        rusqlite::params![body, id],
                    )
                    .map_err(|e| {
                        CoreError::Storage(StorageErrorKind::Other(format!("backfill UPDATE: {e}")))
                    })?;
                    updated += 1;
                }
            }
            Ok(())
        })?;
        Ok(updated)
    }

    /// One-shot startup helper: populate `envelope_id` for any row whose
    /// column is NULL (pre-1.H rows). Decodes `body_blob`, extracts the
    /// envelope id, writes it in place. Skips rows whose blob fails to
    /// decode. Wrapped in a single transaction so all N updates commit
    /// atomically. Returns the number of rows backfilled. Idempotent.
    pub(crate) fn backfill_envelope_id(&self) -> Result<u64> {
        let candidates: Vec<(i64, Vec<u8>)> = self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, body_blob FROM messages WHERE envelope_id IS NULL ORDER BY id ASC",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare backfill_envelope_id: {e}"
                    )))
                })?;
            let it = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "query backfill_envelope_id: {e}"
                    )))
                })?;
            let v: std::result::Result<Vec<_>, _> = it.collect();
            v.map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "collect backfill_envelope_id: {e}"
                )))
            })
        })?;

        if candidates.is_empty() {
            return Ok(0);
        }

        let mut updated = 0u64;
        self.pool.transaction(|tx| {
            for (row_id, blob) in &candidates {
                let env = match crate::envelope::Envelope::decode(blob) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(
                            row_id = *row_id,
                            error = %e,
                            "backfill_envelope_id: skipping row whose body_blob \
                             failed to decode"
                        );
                        continue;
                    }
                };
                match tx.execute(
                    "UPDATE messages SET envelope_id = ?1 WHERE id = ?2",
                    rusqlite::params![&env.id.0[..], row_id],
                ) {
                    Ok(_) => updated += 1,
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
                    {
                        // Pre-existing duplicate (group_id, envelope_id) —
                        // keep the lowest row id, delete this one. The SELECT
                        // above uses ORDER BY id ASC, so earlier rows are
                        // processed first and the UPDATE on the lower-id row
                        // always succeeds before we reach duplicates here.
                        // The ORDER BY makes this "earliest wins" invariant
                        // enforceable rather than relying on SQLite's undefined
                        // natural-scan order.
                        tracing::warn!(
                            row_id = *row_id,
                            "backfill_envelope_id: duplicate (group_id, envelope_id) \
                             detected; deleting higher-id duplicate"
                        );
                        tx.execute(
                            "DELETE FROM messages WHERE id = ?1",
                            rusqlite::params![row_id],
                        )
                        .map_err(|e| {
                            CoreError::Storage(StorageErrorKind::Other(format!(
                                "backfill dedupe delete: {e}"
                            )))
                        })?;
                    }
                    Err(e) => {
                        return Err(CoreError::Storage(StorageErrorKind::Other(format!(
                            "backfill UPDATE: {e}"
                        ))));
                    }
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
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("prune_keep_last: {e}")))
                })?;
            Ok(u64::try_from(n).unwrap_or(0))
        })
    }

    /// Delete all messages for `group_id` inside the caller's transaction.
    /// The `messages_ad_text` AFTER DELETE trigger auto-syncs the FTS5 index.
    /// Idempotent — no error if no rows match.
    pub(crate) fn delete_by_group_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        group_id: &[u8],
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM messages WHERE group_id = ?1",
            rusqlite::params![group_id],
        )
        .map_err(|e| {
            CoreError::Storage(StorageErrorKind::Other(format!(
                "delete messages by group: {e}"
            )))
        })?;
        Ok(())
    }

    /// Return the most recently inserted message in `group_id`, or
    /// `None` if the group is empty. Used by `dispatch::list_contacts`
    /// to populate `ContactSummary::last_message_preview` and
    /// `last_ts_recv`.
    ///
    /// SQL plan: the existing `(group_id, id)` index from migration 0001
    /// makes this an index-scan with `LIMIT 1` — constant cost regardless
    /// of group size.
    pub fn latest_for_group(&self, group_id: &[u8]) -> Result<Option<StoredMessage>> {
        self.pool.with(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, group_id, sender, kind, body_blob, ts, delivered_at, \
                            mls_generation, ts_daemon_recv \
                     FROM messages \
                     WHERE group_id = ?1 \
                     ORDER BY id DESC \
                     LIMIT 1",
                )
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "prepare latest_for_group: {e}"
                    )))
                })?;
            let mut rows = stmt
                .query_map(rusqlite::params![group_id], |r| {
                    Ok(StoredMessage {
                        id: r.get(0)?,
                        group_id: r.get(1)?,
                        sender: r.get(2)?,
                        kind: r.get(3)?,
                        body_blob: r.get(4)?,
                        ts: r.get(5)?,
                        delivered_at: r.get(6)?,
                        mls_generation: r.get(7)?,
                        ts_daemon_recv: r.get(8)?,
                    })
                })
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "query latest_for_group: {e}"
                    )))
                })?;
            match rows.next() {
                None => Ok(None),
                Some(Ok(r)) => Ok(Some(r)),
                Some(Err(e)) => Err(CoreError::Storage(StorageErrorKind::Other(format!(
                    "collect latest_for_group: {e}"
                )))),
            }
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

    /// #200: the ACK carries a 16-byte envelope id, not a SQLite row id, so
    /// `mark_delivered(row_id, …)` was uncallable from the ack path — which is
    /// why it had no production caller and `delivered_at` was NULL for every
    /// message. Marking must be addressable by the id the peer actually sends.
    #[test]
    fn mark_delivered_by_envelope_id_sets_timestamp() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let env = sample_envelope("hi");
        repo.insert(InsertParams {
            group_id: &[0x44; 32],
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: env.ts,
        })
        .unwrap();

        let marked = repo
            .mark_delivered_by_envelope_id(&env.id.0, &[0u8; 32], 4242)
            .unwrap();
        assert!(marked, "the row should have been found by its envelope id");
        assert_eq!(
            repo.recent(&[0x44; 32], 10).unwrap()[0].delivered_at,
            Some(4242)
        );

        // An unknown envelope id marks nothing and is not an error: an ACK can
        // legitimately arrive for a message this node no longer holds.
        let unknown = repo
            .mark_delivered_by_envelope_id(&[0xEE; 16], &[0u8; 32], 5555)
            .unwrap();
        assert!(!unknown);
    }

    /// #200 review (P1, security): a delivery receipt is only meaningful for a
    /// message we sent. Without the sender scope an authenticated peer could
    /// ACK the envelope id of a message *it* sent us and forge a Delivered
    /// receipt on an incoming row.
    #[test]
    fn mark_delivered_refuses_a_row_this_node_did_not_send() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let me = [0x11u8; 32];
        let peer = [0x22u8; 32];

        // An INCOMING message: sender is the peer, not us.
        let env = sample_envelope("from-them");
        repo.insert(InsertParams {
            group_id: &[0x55; 32],
            sender: &peer,
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: env.ts,
        })
        .unwrap();

        // The peer ACKs the id of the message it sent us.
        let forged = repo
            .mark_delivered_by_envelope_id(&env.id.0, &me, 7777)
            .unwrap();
        assert!(!forged, "an incoming row must not be markable as delivered");
        assert_eq!(repo.recent(&[0x55; 32], 10).unwrap()[0].delivered_at, None);
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })?;
                let it = stmt
                    .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })?;
                let v: std::result::Result<Vec<_>, _> = it.collect();
                v.map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
            })
            .unwrap();
        // Highest mls_generation first → "third-yet-older-gen" (gen=5),
        // then "second" (gen=3), then "first-but-newer-gen" (gen=2).
        assert_eq!(rows.len(), 3);
        let bodies: Vec<String> = pool
            .with(|c| {
                let mut stmt = c
                    .prepare("SELECT body_text FROM messages WHERE id = ?1")
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })?;
                let mut out = Vec::new();
                for id in &rows {
                    let body: String = stmt
                        .query_row(rusqlite::params![id], |r| r.get(0))
                        .map_err(|e| {
                            crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                        })?;
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })?;
                let it = stmt
                    .query_map(rusqlite::params![&gid[..]], |r| r.get::<_, i64>(0))
                    .map_err(|e| {
                        crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                    })?;
                let v: std::result::Result<Vec<_>, _> = it.collect();
                v.map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
        // Migration 0007 adds envelope_id with a BEFORE-INSERT trigger that
        // requires the 16-byte value; provide it here so the INSERT succeeds
        // even though we're simulating a legacy row (body_text NULL).
        let env = sample_envelope("legacy hello world");
        let blob = env.encode().unwrap();
        let sender = [0u8; 32];
        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages \
                     (group_id, sender, envelope_id, kind, body_blob, body_text, ts, \
                      mls_generation, ts_daemon_recv) \
                 VALUES (?1, ?2, ?3, 'text', ?4, NULL, ?5, 0, 0)",
                rusqlite::params![&gid[..], &sender[..], &env.id.0[..], blob, env.ts],
            )
            .map_err(|e| {
                crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
            })?;
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
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

    #[test]
    fn backfill_envelope_id_populates_null_rows_from_body_blob() {
        let pool = Pool::in_memory();
        let gid = [0x70u8; 32];
        let sender = [0x71u8; 32];

        // Insert a row with body_blob set but envelope_id NULL — simulates
        // a pre-1.H row. Bypass the trigger by inserting envelope_id as a
        // dummy 16-byte value, then NULL it out (the trigger fires on
        // INSERT, not UPDATE).
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 1_700_000_000,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "hi".into() },
        };
        let expected_id = env.id.0;

        pool.with_mut(|c| {
            c.execute(
                "INSERT INTO messages \
                 (group_id, sender, envelope_id, ts, ts_daemon_recv, \
                  mls_generation, kind, body_blob, body_text) \
                 VALUES (?1, ?2, ?3, ?4, 0, 0, 'text', ?5, 'hi')",
                rusqlite::params![
                    &gid[..],
                    &sender[..],
                    &[0u8; 16][..], // dummy 16 bytes to satisfy trigger
                    env.ts,
                    env.encode().unwrap(),
                ],
            )
            .unwrap();
            // Now NULL the envelope_id (UPDATE bypasses the BEFORE-INSERT trigger).
            c.execute(
                "UPDATE messages SET envelope_id = NULL WHERE group_id = ?1",
                rusqlite::params![&gid[..]],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

        let n = MessageRepo::new(&pool).backfill_envelope_id().unwrap();
        assert_eq!(n, 1, "exactly one row backfilled");

        let got: Vec<u8> = pool
            .with(|c| {
                c.query_row(
                    "SELECT envelope_id FROM messages WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(
            got, expected_id,
            "backfilled envelope_id must match body_blob"
        );
    }

    #[test]
    fn insert_populates_envelope_id_column() {
        let pool = Pool::in_memory();
        let gid = [0x74u8; 32];
        let sender = [0x75u8; 32];
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "x".into() },
        };
        let expected = env.id.0;

        let repo = MessageRepo::new(&pool);
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: 0,
        })
        .unwrap();

        let got: Vec<u8> = pool
            .with(|c| {
                c.query_row(
                    "SELECT envelope_id FROM messages WHERE group_id = ?1",
                    rusqlite::params![&gid[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(crate::storage::StorageErrorKind::Other(
                        e.to_string(),
                    ))
                })
            })
            .unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn insert_duplicate_envelope_id_returns_duplicate_message_error() {
        use crate::error::CoreError;
        use crate::storage::StorageErrorKind;

        let pool = Pool::in_memory();
        let gid = [0x76u8; 32];
        let sender = [0x77u8; 32];
        let env = crate::envelope::Envelope {
            v: 1,
            id: crate::envelope::MessageId::generate(),
            ts: 0,
            reply_to: None,
            kind: crate::envelope::Kind::Text { body: "y".into() },
        };

        let repo = MessageRepo::new(&pool);
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &sender,
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: 0,
        })
        .unwrap();

        let err = repo
            .insert(InsertParams {
                group_id: &gid,
                sender: &sender,
                envelope: &env, // same envelope.id as above
                mls_generation: 1,
                ts_daemon_recv: 1,
            })
            .unwrap_err();

        assert!(
            matches!(err, CoreError::Storage(StorageErrorKind::DuplicateMessage)),
            "expected DuplicateMessage, got {err:?}"
        );
    }

    #[test]
    fn backfill_envelope_id_is_idempotent() {
        let pool = Pool::in_memory();
        let gid = [0x72u8; 32];
        let repo = MessageRepo::new(&pool);
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0x73u8; 32],
            envelope: &crate::envelope::Envelope {
                v: 1,
                id: crate::envelope::MessageId::generate(),
                ts: 0,
                reply_to: None,
                kind: crate::envelope::Kind::Text { body: "a".into() },
            },
            mls_generation: 0,
            ts_daemon_recv: 0,
        })
        .unwrap();
        // Row already has envelope_id populated by insert (Task 4 wires that).
        // Backfill must do nothing.
        let n = repo.backfill_envelope_id().unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn latest_for_group_returns_max_id_row() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);

        let group_id = [0xAA; 32];
        let env_a = sample_envelope("first");
        let env_b = sample_envelope("second");
        let pk = [1u8; 32];
        repo.insert(InsertParams {
            group_id: &group_id,
            sender: &pk,
            envelope: &env_a,
            mls_generation: 0,
            ts_daemon_recv: 100,
        })
        .unwrap();
        repo.insert(InsertParams {
            group_id: &group_id,
            sender: &pk,
            envelope: &env_b,
            mls_generation: 0,
            ts_daemon_recv: 200,
        })
        .unwrap();

        let row = repo
            .latest_for_group(&group_id)
            .unwrap()
            .expect("at least one row");
        assert_eq!(row.ts_daemon_recv, 200);
    }

    #[test]
    fn latest_for_group_returns_none_when_empty() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        assert!(repo.latest_for_group(&[0xBB; 32]).unwrap().is_none());
    }

    #[test]
    fn recent_before_excludes_cursor_and_orders_descending() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xCC; 32];
        let mut row_ids = Vec::new();
        for i in 0..10 {
            let mut env = sample_envelope(&format!("m{i}"));
            env.ts = 1000 + i as i64;
            let id = repo
                .insert(InsertParams {
                    group_id: &gid,
                    sender: &[0u8; 32],
                    envelope: &env,
                    mls_generation: 0,
                    ts_daemon_recv: env.ts,
                })
                .unwrap();
            row_ids.push(id);
        }
        let cursor = row_ids[6];
        let page = repo.recent_before(&gid, cursor, 5).unwrap();
        assert_eq!(page.len(), 5);
        assert!(page.iter().all(|m| m.id != cursor));
        assert!(page.iter().all(|m| m.id < cursor));
        let ids: Vec<i64> = page.iter().map(|m| m.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(ids, sorted);
    }

    #[test]
    fn recent_before_with_orphan_cursor_returns_older_rows() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xDD; 32];
        let env = sample_envelope("only-row");
        repo.insert(InsertParams {
            group_id: &gid,
            sender: &[0u8; 32],
            envelope: &env,
            mls_generation: 0,
            ts_daemon_recv: env.ts,
        })
        .unwrap();
        let page = repo.recent_before(&gid, 999_999, 10).unwrap();
        assert_eq!(page.len(), 1, "should return rows older than orphan cursor");
    }

    #[test]
    fn delete_by_group_in_tx_removes_rows_and_syncs_fts() {
        let pool = Pool::in_memory();
        let repo = MessageRepo::new(&pool);
        let gid = [0xEEu8; 32];
        let other_gid = [0xFFu8; 32];

        // Insert messages in the target group and one in an unrelated group.
        for body in &["first message", "second message"] {
            repo.insert(InsertParams {
                group_id: &gid,
                sender: &[0u8; 32],
                envelope: &sample_envelope(body),
                mls_generation: 0,
                ts_daemon_recv: 0,
            })
            .unwrap();
        }
        repo.insert(InsertParams {
            group_id: &other_gid,
            sender: &[0u8; 32],
            envelope: &sample_envelope("other group"),
            mls_generation: 0,
            ts_daemon_recv: 0,
        })
        .unwrap();

        pool.transaction(|tx| repo.delete_by_group_in_tx(tx, &gid))
            .unwrap();

        // Target group rows gone; other group row untouched.
        assert!(repo.recent(&gid, 10).unwrap().is_empty());
        assert_eq!(repo.recent(&other_gid, 10).unwrap().len(), 1);

        // FTS index consistent: no hits for the deleted group's content.
        let fts_hits: i64 = pool
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'first'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| {
                    crate::error::CoreError::Storage(StorageErrorKind::Other(e.to_string()))
                })
            })
            .unwrap();
        assert_eq!(fts_hits, 0, "ad trigger must cascade FTS deletes");
    }
}
