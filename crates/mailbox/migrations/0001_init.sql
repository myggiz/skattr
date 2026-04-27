CREATE TABLE deposits (
  deposit_id     BLOB PRIMARY KEY,
  recipient_hash BLOB NOT NULL,
  ciphertext     BLOB NOT NULL,
  deposited_at   INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL
);
CREATE INDEX idx_deposits_recipient ON deposits(recipient_hash, deposited_at);
CREATE INDEX idx_deposits_expiry    ON deposits(expires_at);

CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
