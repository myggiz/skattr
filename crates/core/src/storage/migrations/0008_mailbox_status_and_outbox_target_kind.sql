-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr storage schema, version 8.
-- Phase 2.B mailbox client: status tracking on `mailboxes`, target-kind
-- + mailbox FK on `outbox` for the direct→mailbox fallback path.

INSERT OR IGNORE INTO schema_version (version) VALUES (8);

ALTER TABLE mailboxes ADD COLUMN status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (status IN ('unknown','reachable','unreachable',
                      'rate_limited','pending_removal','removed'));
ALTER TABLE mailboxes ADD COLUMN last_poll_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN last_error_kind TEXT;

ALTER TABLE outbox ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'direct'
    CHECK (target_kind IN ('direct','mailbox'));
ALTER TABLE outbox ADD COLUMN mailbox_id INTEGER NOT NULL DEFAULT 0;

DROP INDEX IF EXISTS idx_outbox_target_message_id;
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_kind_mailbox
    ON outbox(target, message_id, target_kind, mailbox_id);
