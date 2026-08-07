-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0014: passphrase change audit log
--
-- Append-only record of ChangePassphrase outcomes. Surfaced as
-- "Last changed" in Settings → Identity. Rows are NEVER deleted by the
-- retention sweep — small (one row per passphrase change) and auditable.

CREATE TABLE IF NOT EXISTS passphrase_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_unix     INTEGER NOT NULL,
    outcome     TEXT NOT NULL CHECK(outcome IN ('changed','rolled_back','recovered'))
);
