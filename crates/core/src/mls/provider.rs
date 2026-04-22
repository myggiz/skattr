// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `OpenMlsRustCrypto` wrapper with snapshot / load for persistence.

use crate::error::Result;

/// Crypto + storage provider for OpenMLS. Uses `OpenMlsRustCrypto`
/// under the hood (in-memory `MemoryStorage`) plus a custom snapshot
/// path for persistence.
pub(crate) struct MlsProvider {
    _private: (),
}

impl MlsProvider {
    pub(crate) fn new() -> Self {
        todo!("Task 3")
    }

    pub(crate) fn snapshot(&self) -> Result<Vec<u8>> {
        todo!("Task 3")
    }

    pub(crate) fn load(_bytes: &[u8]) -> Result<Self> {
        todo!("Task 3")
    }
}
