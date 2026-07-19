-- First-contact durable Welcome re-send (#93). One row per pending first
-- contact where WE are the committer/invitee and the peer has not yet Ack'd
-- the Welcome. Deleted on Ack (peer joined) or on RemoveContact.
CREATE TABLE pending_welcomes (
    peer_pubkey    BLOB PRIMARY KEY NOT NULL,   -- responder identity pubkey (32B)
    group_id       BLOB NOT NULL,               -- genesis group id
    welcome_bytes  BLOB NOT NULL,               -- exact Welcome message to re-send
    next_retry_at  INTEGER NOT NULL,            -- ms; due-time for the next send
    attempts       INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL
);
CREATE INDEX idx_pending_welcomes_due ON pending_welcomes(next_retry_at);
