-- 0021: durable delivery outcome for outgoing messages.
--
-- dismissed_at  — the user dismissed a failed send. The row STAYS in
--                 history and in FTS; this only hides the actions and
--                 greys the bubble. Not derivable from anything else.
-- failed_reason — why the daemon gave up, stored rather than derived so
--                 the remedy survives a restart. A reason computed at
--                 read time would come back as a bare "failed".
--
-- Both nullable, mirroring the existing messages.delivered_at.
ALTER TABLE messages ADD COLUMN dismissed_at INTEGER;
ALTER TABLE messages ADD COLUMN failed_reason TEXT;
