// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Repository for MLS group state blobs.
//!
//! MLS state is opaque to `core` beyond "a blob per group." We persist
//! it transactionally so a crash between a `process_message` call and
//! our own follow-up writes can't leave us with a group whose MLS state
//! has advanced but whose visible state hasn't.

use crate::error::Result;

/// Group state repository.
#[derive(Debug)]
pub struct MlsGroupRepo {
    _private: (),
}

impl MlsGroupRepo {
    /// Save or update the serialized MLS state for a group.
    pub fn put(&self, _group_id: &[u8], _state_blob: &[u8], _epoch: u64) -> Result<()> {
        todo!("INSERT OR REPLACE INTO mls_groups …")
    }

    /// Load the serialized state for a group.
    pub fn get(&self, _group_id: &[u8]) -> Result<Option<Vec<u8>>> {
        todo!("SELECT state_blob FROM mls_groups WHERE group_id = ?")
    }

    /// Enumerate all persisted groups.
    pub fn list(&self) -> Result<Vec<(Vec<u8>, u64)>> {
        todo!("SELECT group_id, epoch FROM mls_groups")
    }
}
