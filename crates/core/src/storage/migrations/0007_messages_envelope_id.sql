-- Phase 1.H: durable (group_id, envelope_id) uniqueness.
-- The column is nullable because SQLite can't ALTER a column to NOT NULL
-- mid-life; the trigger below enforces 16-byte shape on every new INSERT,
-- and a startup-time Rust backfill populates any NULLs from pre-1.H rows
-- (see MessageRepo::backfill_envelope_id).
ALTER TABLE messages ADD COLUMN envelope_id BLOB;

CREATE TRIGGER IF NOT EXISTS messages_envelope_id_shape
BEFORE INSERT ON messages
WHEN new.envelope_id IS NULL OR length(new.envelope_id) <> 16
BEGIN
    SELECT RAISE(ABORT, 'envelope_id must be 16 bytes');
END;

-- NULLs compare distinct by default in SQLite, so pre-backfill legacy
-- rows don't collide. Once backfill runs, the constraint becomes
-- meaningful. See spec §L1.a.
CREATE UNIQUE INDEX IF NOT EXISTS messages_group_envelope_uniq
    ON messages(group_id, envelope_id);
