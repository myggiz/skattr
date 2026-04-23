-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr storage schema, version 5.
-- Bind every contact row to its 2-member MLS group so SendMessage /
-- RecentMessages can cross "contact pubkey -> MLS group" in one join.
-- Phase 1 has no production users; the empty default is cosmetic and
-- AddContact is the only insert path, so every row will carry a real
-- group_id.

INSERT OR IGNORE INTO schema_version (version) VALUES (5);

ALTER TABLE contacts ADD COLUMN group_id BLOB NOT NULL DEFAULT X'';
CREATE INDEX IF NOT EXISTS idx_contacts_group_id ON contacts(group_id);
