-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 9.
-- Phase 2.B self-card version counter for RotateOnion / AddMailbox /
-- RemoveMailbox publishers.

INSERT OR IGNORE INTO schema_version (version) VALUES (9);

CREATE TABLE IF NOT EXISTS self_card_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL DEFAULT 0
);

INSERT OR IGNORE INTO self_card_state (id, version) VALUES (1, 0);
