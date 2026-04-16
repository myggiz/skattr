// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon configuration, loaded from TOML.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Top-level daemon config.
///
/// Loaded from `~/.config/skattr/config.toml` by default; the CLI may
/// override the path via `--config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory for persistent state (SQLite, Arti data, HS keys).
    pub data_dir: PathBuf,
    /// Unix socket / named pipe path for CLI↔daemon IPC.
    #[serde(default)]
    pub ipc_socket: Option<PathBuf>,
    /// Logging filter, e.g. `skattr_core=debug,arti=info`.
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
}

fn default_log_filter() -> String {
    "skattr_core=info,arti=warn".into()
}

impl Config {
    /// Load config from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text)
            .map_err(|e| CoreError::Config(format!("parse {}: {e}", path.display())))
    }

    /// Default config for a brand-new install — uses XDG directories.
    pub fn defaults() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("net", "myggiz", "skattr")
            .ok_or_else(|| CoreError::Config("no home directory".into()))?;
        Ok(Self {
            data_dir: dirs.data_dir().to_path_buf(),
            ipc_socket: None,
            log_filter: default_log_filter(),
        })
    }
}
