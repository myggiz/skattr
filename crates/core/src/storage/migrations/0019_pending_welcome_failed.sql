-- SPDX-License-Identifier: GPL-3.0-or-later
-- #107: a first-contact Welcome that never Ack'd within MAX_WELCOME_AGE is
-- marked failed. The row is kept (is_pending stays true → contact stays
-- PendingJoin, never mis-rendered Active) but the sweep no longer retries it.
ALTER TABLE pending_welcomes ADD COLUMN failed INTEGER NOT NULL DEFAULT 0;
