-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 4.
-- Add per-message id to the outbox so the delivery layer can
-- correlate inbound ACKs to rows without a separate lookup table
-- and so enqueues are idempotent per (target, message_id).

INSERT OR IGNORE INTO schema_version (version) VALUES (4);

ALTER TABLE outbox
    ADD COLUMN message_id BLOB NOT NULL
    DEFAULT (x'00000000000000000000000000000000');

CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_target_message_id
    ON outbox(target, message_id);
