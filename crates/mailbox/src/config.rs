// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox server configuration. Filled in by Task 5.

#![allow(missing_docs)]

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct MailboxConfig {
    pub data_dir: PathBuf,
}
