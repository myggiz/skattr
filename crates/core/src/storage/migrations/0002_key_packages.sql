-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr schema migration 0002: MLS KeyPackages
--
-- Tracks KeyPackages we've generated for peers (direction = 'ours') and
-- KeyPackages we've received from peers (direction = 'theirs'; Phase 2
-- only). 1.C always inserts 'ours'. Single-use enforcement is 1.D's job.

CREATE TABLE IF NOT EXISTS key_packages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kp_hash BLOB NOT NULL UNIQUE,
    kp_bytes BLOB NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('ours', 'theirs')),
    consumed INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_key_packages_hash ON key_packages(kp_hash);
CREATE INDEX IF NOT EXISTS idx_key_packages_direction ON key_packages(direction);
