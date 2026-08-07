-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0012: add provider_snapshot to outstanding_invites
--
-- Adds a `provider_snapshot` column to `outstanding_invites` so the
-- inviter can restore the MLS provider (with the KP init private key) when
-- processing an inbound Welcome message via Group::join_from_welcome.
-- Without the provider snapshot the init key is lost and the Welcome cannot
-- be processed.  Existing rows (pre-migration) get NULL which is handled by
-- `dispatch_welcome_inner` as an error.

ALTER TABLE outstanding_invites ADD COLUMN provider_snapshot BLOB;
