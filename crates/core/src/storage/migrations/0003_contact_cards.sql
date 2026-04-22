-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- skattr schema migration 0003: contact cards
--
-- Stores verified ContactCards per contact, keyed by (contact_id, version).
-- Full history is preserved so rotation (Phase 2) can audit previous
-- cards and grace-period overlaps. `latest_card` queries for the
-- highest version; `put_card` rejects any version that isn't strictly
-- greater than the stored max.

CREATE TABLE IF NOT EXISTS contact_cards (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    card_blob BLOB NOT NULL,
    verified_at INTEGER NOT NULL,
    UNIQUE (contact_id, version)
);

CREATE INDEX IF NOT EXISTS idx_contact_cards_latest
  ON contact_cards(contact_id, version DESC);
