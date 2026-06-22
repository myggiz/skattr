-- Phase 3.C: sender-side per-chunk mailbox-deposit state for offline transfer.
-- Small rows, no payload (chunk bytes live in ChunkStore). next_retry_at is in
-- milliseconds, matching the sweep clock (now_unix_millis).
CREATE TABLE IF NOT EXISTS attachment_deposits (
    attachment_id BLOB NOT NULL,
    chunk_index   INTEGER NOT NULL,
    recipient     BLOB NOT NULL,            -- recipient identity pubkey (32 bytes)
    attempts      INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL,         -- ms since epoch; due when <= now
    status        TEXT NOT NULL CHECK (status IN ('pending','deposited')) DEFAULT 'pending',
    PRIMARY KEY (attachment_id, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_attachment_deposits_due
    ON attachment_deposits (status, next_retry_at);

-- Phase 3.C receiver: store the inbound sender's pubkey alongside the
-- manifest so finalize_offline can populate Event::AttachmentReceived.contact
-- without joining the messages table. NULL for outbound ('out') rows.
ALTER TABLE attachments ADD COLUMN peer BLOB;
