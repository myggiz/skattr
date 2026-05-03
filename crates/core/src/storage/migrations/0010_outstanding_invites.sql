-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz AB
--
-- Skattr schema migration 0010: outstanding invites
--
-- Persists the per-invite PSK on the inviter's side so that when the
-- consumer's Welcome message arrives the inviter can join the new MLS
-- group via Group::join_from_welcome (which requires the same PSK on
-- both sides). Rows are removed by `mark_consumed` (after zeroizing
-- the psk column) or `purge_expired` (after expires_at lapses).

CREATE TABLE IF NOT EXISTS outstanding_invites (
    kp_hash      BLOB PRIMARY KEY,
    psk          BLOB NOT NULL,
    inviter_kp   BLOB NOT NULL,
    expires_at   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outstanding_invites_expires
    ON outstanding_invites(expires_at);
