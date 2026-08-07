-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 1.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

INSERT OR IGNORE INTO schema_version (version) VALUES (1);

CREATE TABLE IF NOT EXISTS identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    public_key BLOB NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_pubkey BLOB NOT NULL UNIQUE,
    display_name TEXT,
    added_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS onion_addresses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    address TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    is_current INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_onion_contact ON onion_addresses(contact_id);

CREATE TABLE IF NOT EXISTS mls_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id BLOB NOT NULL UNIQUE,
    state_blob BLOB NOT NULL,
    epoch INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id BLOB NOT NULL,
    sender BLOB NOT NULL,
    kind TEXT NOT NULL,
    body_blob BLOB,
    ts INTEGER NOT NULL,
    delivered_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_messages_group_ts ON messages(group_id, ts);
CREATE INDEX IF NOT EXISTS idx_messages_sender ON messages(sender);

-- Full-text search over the body (text-kind messages only).
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    body,
    content='messages',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    target BLOB NOT NULL,
    payload BLOB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outbox_due ON outbox(next_retry_at);

CREATE TABLE IF NOT EXISTS mailboxes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    onion TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('mine', 'theirs'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_mailboxes_onion_role ON mailboxes(onion, role);

-- Dedup window for received messages: (sender, message_id) with TTL.
CREATE TABLE IF NOT EXISTS seen_messages (
    sender BLOB NOT NULL,
    message_id BLOB NOT NULL,
    seen_at INTEGER NOT NULL,
    PRIMARY KEY (sender, message_id)
);

CREATE INDEX IF NOT EXISTS idx_seen_age ON seen_messages(seen_at);
