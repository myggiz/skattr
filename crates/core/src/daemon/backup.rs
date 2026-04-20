// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Public re-exports for backup export / import. The implementations
//! live in `storage::backup` (crate-private); this module surfaces the
//! two helpers through the public `daemon` API so the CLI can call them
//! without widening `storage`'s visibility.

use std::path::Path;

use crate::error::Result;
use crate::identity::Seed;

/// Write a backup archive to `out_path`. See [`crate::storage::backup::export_backup`].
pub fn export_backup(data_dir: &Path, out_path: &Path, seed: &Seed) -> Result<()> {
    crate::storage::backup::export_backup(data_dir, out_path, seed)
}

/// Read a backup archive from `archive_path` and extract into `data_dir`.
/// See [`crate::storage::backup::import_backup`].
pub fn import_backup(archive_path: &Path, data_dir: &Path, seed: &Seed) -> Result<()> {
    crate::storage::backup::import_backup(archive_path, data_dir, seed)
}
