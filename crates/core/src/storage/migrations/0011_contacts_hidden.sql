-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0011: soft-delete column on contacts
--
-- `hidden = 1` removes the contact from the default ListContacts view
-- but preserves MLS group state, messages, mailboxes, and read cursors
-- so a future "Show archived" UX can restore. Existing rows default
-- to 0 (visible).

ALTER TABLE contacts ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_contacts_hidden ON contacts(hidden);
