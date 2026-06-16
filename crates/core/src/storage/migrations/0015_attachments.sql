-- Phase 3.A: attachment transfer state.
CREATE TABLE IF NOT EXISTS attachments (
    attachment_id BLOB PRIMARY KEY,            -- 16 random bytes
    direction     TEXT NOT NULL CHECK (direction IN ('out', 'in')),
    manifest      BLOB NOT NULL,               -- CBOR AttachmentManifest
    total_chunks  INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('pending', 'complete', 'failed')),
    created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS attachment_chunks (
    attachment_id BLOB NOT NULL,
    chunk_index   INTEGER NOT NULL,
    received      INTEGER NOT NULL DEFAULT 0,  -- 0/1
    PRIMARY KEY (attachment_id, chunk_index)
);
