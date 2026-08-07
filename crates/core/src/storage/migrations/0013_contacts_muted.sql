-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0013: per-contact mute flag
--
-- `muted = 1` suppresses desktop notifications and the unread badge for
-- this contact's group. Default 0 (notifications fire). The UI surfaces
-- a bell icon next to muted contacts.

ALTER TABLE contacts ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;
