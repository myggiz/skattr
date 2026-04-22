// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Repository for MLS KeyPackages (ours to publish + theirs to consume).

use crate::error::Result;
use crate::storage::Pool;

pub struct KeyPackageRepo<'p> {
    _pool: &'p Pool,
}

impl<'p> KeyPackageRepo<'p> {
    pub(crate) fn new(_pool: &'p Pool) -> Self {
        todo!("Task 2")
    }

    pub(crate) fn insert(
        &self,
        _hash: &[u8; 32],
        _bytes: &[u8],
        _direction: &str,
    ) -> Result<()> {
        todo!("Task 2")
    }

    pub(crate) fn get(&self, _hash: &[u8; 32]) -> Result<Option<(Vec<u8>, bool)>> {
        todo!("Task 2")
    }

    pub(crate) fn mark_consumed(&self, _hash: &[u8; 32]) -> Result<()> {
        todo!("Task 2")
    }
}
