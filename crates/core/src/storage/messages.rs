// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Message history repository, with FTS5 virtual-table search.

use crate::envelope::Envelope;
use crate::error::Result;

/// Opaque stored-message handle. Public fields are a subset of
/// [`Envelope`] plus local-only columns (delivered_at, etc.).
#[derive(Debug, Clone)]
pub struct StoredMessage {
    /// SQLite rowid.
    pub id: i64,
    /// Sender pubkey, hex-encoded.
    pub sender_hex: String,
    /// Envelope payload (CBOR).
    pub body_blob: Vec<u8>,
    /// Local-clock receive timestamp.
    pub received_at: i64,
    /// Whether the message has been acknowledged by the recipient.
    pub acked: bool,
}

/// Message repository.
#[derive(Debug)]
pub struct MessageRepo {
    _private: (),
}

impl MessageRepo {
    /// Insert a newly-received message.
    pub fn insert(&self, _envelope: &Envelope, _sender_hex: &str) -> Result<i64> {
        todo!("INSERT into messages; trigger updates FTS5 index for text kinds")
    }

    /// Fetch the most recent N messages for a conversation.
    pub fn recent(&self, _group_id: &[u8], _limit: usize) -> Result<Vec<StoredMessage>> {
        todo!("SELECT … WHERE group_id = ? ORDER BY ts DESC LIMIT ?")
    }

    /// Full-text search across message bodies.
    pub fn search(&self, _query: &str, _limit: usize) -> Result<Vec<StoredMessage>> {
        todo!("SELECT … FROM messages_fts WHERE messages_fts MATCH ? …")
    }
}
