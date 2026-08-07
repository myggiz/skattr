-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 6.
-- Phase 1.G: wire FTS5 over messages, persist mls_generation +
-- ts_daemon_recv, add read_state cursor.

ALTER TABLE messages ADD COLUMN body_text TEXT;
ALTER TABLE messages ADD COLUMN mls_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN ts_daemon_recv INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_messages_group_gen
    ON messages(group_id, mls_generation DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_messages_ts_recv
    ON messages(ts_daemon_recv);

DROP TABLE IF EXISTS messages_fts;
CREATE VIRTUAL TABLE messages_fts USING fts5(
    body_text,
    content='messages',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS messages_ai_text
    AFTER INSERT ON messages
    WHEN NEW.kind = 'text' AND NEW.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(rowid, body_text)
        VALUES (NEW.id, NEW.body_text);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad_text
    AFTER DELETE ON messages
    WHEN OLD.kind = 'text' AND OLD.body_text IS NOT NULL
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        VALUES('delete', OLD.id, OLD.body_text);
END;

CREATE TRIGGER IF NOT EXISTS messages_au_text
    AFTER UPDATE OF body_text, kind ON messages
BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, body_text)
        SELECT 'delete', OLD.id, OLD.body_text
        WHERE OLD.kind = 'text' AND OLD.body_text IS NOT NULL;
    INSERT INTO messages_fts(rowid, body_text)
        SELECT NEW.id, NEW.body_text
        WHERE NEW.kind = 'text' AND NEW.body_text IS NOT NULL;
END;

CREATE TABLE IF NOT EXISTS read_state (
    group_id BLOB PRIMARY KEY,
    last_read_message_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

INSERT OR REPLACE INTO schema_version (version) VALUES (6);
