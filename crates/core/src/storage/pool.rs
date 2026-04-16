// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Connection pool.
//!
//! `rusqlite::Connection` is `!Send` + `!Sync`, so a pool of short-lived
//! connections behind a Mutex is the standard approach. A single writer
//! thread avoids WAL starvation under load.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::Result;

/// Very simple connection holder.
///
/// For Phase 0 scaffolding this is a single-connection Mutex; Phase 1
/// upgrades to a proper pool with one writer + N readers.
pub struct Pool {
    inner: Mutex<Connection>,
}

impl Pool {
    /// Open the database at `path`, apply pragmas, run migrations.
    pub fn open(_path: &Path) -> Result<Self> {
        todo!("open Connection, set pragmas, run migrations, wrap in Mutex")
    }

    /// Execute a function with an exclusive lock on the connection.
    pub fn with<F, R>(&self, _f: F) -> Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R>,
    {
        let _ = &self.inner;
        todo!("lock mutex, call f, release")
    }
}
