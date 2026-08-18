-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0020: add `nickname` to outstanding_invites.
--
-- The invite dialog collects an optional nickname, but the daemon discarded it
-- and the resulting contact rendered as raw hex (#174). The name is applied
-- when the invite's Welcome arrives, which may be days after the invite was
-- created and across daemon restarts — so it has to live in the invite row
-- rather than in memory.
--
-- Nullable: existing invites have no nickname, and the field is optional.
-- Local-only, exactly like RenameContact: never sent to the peer.

ALTER TABLE outstanding_invites ADD COLUMN nickname TEXT;
