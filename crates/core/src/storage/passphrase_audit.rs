// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Append-only audit log for `Command::ChangePassphrase` outcomes.
//! Surfaces "Last changed" in Settings → Identity. Rows are not
//! deleted by the retention sweep.

use rusqlite::OptionalExtension;

use crate::error::{CoreError, Result};
use crate::storage::Pool;
use crate::storage::StorageErrorKind;

pub struct PassphraseAuditRepo<'a> {
    pool: &'a Pool,
}

impl<'a> PassphraseAuditRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Append a new audit row. `ts_unix` is wall-clock seconds since
    /// the Unix epoch.
    pub fn append(&self, ts_unix: i64, outcome: AuditOutcome) -> Result<()> {
        self.pool.with(|c| {
            c.execute(
                "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (?1, ?2)",
                rusqlite::params![ts_unix, outcome.as_str()],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!("audit append: {e}")))
            })?;
            Ok(())
        })
    }

    /// Read the most recent audit row's `ts_unix`. Returns `Ok(None)`
    /// if the table is empty.
    pub fn latest_ts(&self) -> Result<Option<i64>> {
        self.pool.with(|c| {
            let row: Option<i64> = c
                .query_row(
                    "SELECT ts_unix FROM passphrase_audit ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!("audit latest: {e}")))
                })?;
            Ok(row)
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuditOutcome {
    Changed,
    RolledBack,
    Recovered,
}

impl AuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::RolledBack => "rolled_back",
            Self::Recovered => "recovered",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn append_then_latest_returns_most_recent() {
        let pool = std::sync::Arc::new(Pool::in_memory());
        let repo = PassphraseAuditRepo::new(&pool);
        assert_eq!(repo.latest_ts().unwrap(), None);
        repo.append(1_000, AuditOutcome::Changed).unwrap();
        repo.append(2_000, AuditOutcome::Recovered).unwrap();
        assert_eq!(repo.latest_ts().unwrap(), Some(2_000));
    }

    #[test]
    fn audit_outcome_strings_match_check_constraint() {
        assert_eq!(AuditOutcome::Changed.as_str(), "changed");
        assert_eq!(AuditOutcome::RolledBack.as_str(), "rolled_back");
        assert_eq!(AuditOutcome::Recovered.as_str(), "recovered");
    }
}
