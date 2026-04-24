// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon configuration, loaded from TOML.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Message history retention settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryConfig {
    /// Days of history to retain. 0 = infinite (default; sweep no-ops).
    #[serde(default)]
    pub retention_days: u32,
}

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
    /// Retention + history settings. Drives the retention sweep.
    #[serde(default)]
    pub history: HistoryConfig,
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
            history: HistoryConfig::default(),
        })
    }

    /// Return the configured `ipc_socket` or a best-effort default
    /// under `$XDG_RUNTIME_DIR/skattr/daemon.sock`, falling back to
    /// `$TMPDIR/skattr/daemon.sock`, then `/tmp/skattr/daemon.sock`.
    pub fn ipc_socket_or_default(&self) -> Result<std::path::PathBuf> {
        if let Some(p) = &self.ipc_socket {
            return Ok(p.clone());
        }
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("TMPDIR").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        Ok(base.join("skattr").join("daemon.sock"))
    }

    /// Load a config with the standard precedence:
    /// `flag > env > file > default`. `flag_data_dir` / `env_data_dir`
    /// / `flag_socket` are CLI-layer overrides and can each be `None`.
    ///
    /// If `file` is `Some` and missing, returns a hard error.
    /// If `file` is `None`, tries
    /// `$XDG_CONFIG_HOME/skattr/config.toml`, then
    /// `$HOME/.config/skattr/config.toml`, then falls back to
    /// defaults (absence of the file is not an error).
    pub fn load_with_precedence(
        file: Option<&std::path::Path>,
        flag_data_dir: Option<&std::path::Path>,
        env_data_dir: Option<&std::path::Path>,
        flag_socket: Option<&std::path::Path>,
    ) -> Result<Self> {
        let mut cfg = match file {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| CoreError::Config(format!("read {}: {e}", p.display())))?;
                toml::from_str(&text)
                    .map_err(|e| CoreError::Config(format!("parse {}: {e}", p.display())))?
            }
            None => {
                let candidates = xdg_config_candidates();
                let mut found: Option<Self> = None;
                for candidate in candidates {
                    if candidate.exists() {
                        let text = std::fs::read_to_string(&candidate).map_err(|e| {
                            CoreError::Config(format!("read {}: {e}", candidate.display()))
                        })?;
                        found = Some(toml::from_str(&text).map_err(|e| {
                            CoreError::Config(format!("parse {}: {e}", candidate.display()))
                        })?);
                        break;
                    }
                }
                found.unwrap_or_else(|| Self::defaults().unwrap_or_else(|_| Self::fallback()))
            }
        };

        // Apply env override.
        if let Some(env) = env_data_dir {
            cfg.data_dir = env.to_path_buf();
        }
        // Apply flag override (highest precedence).
        if let Some(flag) = flag_data_dir {
            cfg.data_dir = flag.to_path_buf();
        }
        if let Some(sock) = flag_socket {
            cfg.ipc_socket = Some(sock.to_path_buf());
        }
        Ok(cfg)
    }

    fn fallback() -> Self {
        Self {
            data_dir: std::path::PathBuf::from("./skattr-data"),
            ipc_socket: None,
            log_filter: default_log_filter(),
            history: HistoryConfig::default(),
        }
    }
}

fn xdg_config_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(
            std::path::PathBuf::from(xdg)
                .join("skattr")
                .join("config.toml"),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(
            std::path::PathBuf::from(home)
                .join(".config")
                .join("skattr")
                .join("config.toml"),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_defaults_when_no_path_and_no_xdg_file() {
        // This test only asserts the call doesn't error; the actual
        // defaults depend on env state at runtime.
        let cfg = Config::load_with_precedence(None, None, None, None).unwrap();
        assert!(!cfg.data_dir.as_os_str().is_empty());
    }

    #[test]
    fn load_explicit_missing_path_is_error() {
        let missing = std::path::PathBuf::from("/nonexistent/skattr/config.toml");
        let err = Config::load_with_precedence(Some(&missing), None, None, None)
            .expect_err("explicit missing path must error");
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn env_data_dir_overrides_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, r#"data_dir = "/file/data""#).unwrap();
        let env_path = tmp.path().join("env_data");
        let cfg =
            Config::load_with_precedence(Some(&cfg_path), None, Some(&env_path), None).unwrap();
        assert_eq!(cfg.data_dir, env_path);
    }

    #[test]
    fn flag_data_dir_overrides_env_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, r#"data_dir = "/file/data""#).unwrap();
        let env_path = tmp.path().join("env_data");
        let flag_path = tmp.path().join("flag_data");
        let cfg =
            Config::load_with_precedence(Some(&cfg_path), Some(&flag_path), Some(&env_path), None)
                .unwrap();
        assert_eq!(cfg.data_dir, flag_path);
    }

    #[test]
    fn file_with_invalid_toml_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, "not = valid = toml").unwrap();
        let err = Config::load_with_precedence(Some(&cfg_path), None, None, None)
            .expect_err("invalid TOML must error");
        assert!(matches!(err, CoreError::Config(_)), "got {err:?}");
    }

    #[test]
    fn history_section_defaults_to_zero_when_absent() {
        let toml = r#"
            data_dir = "/tmp/skattr"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.history.retention_days, 0);
    }

    #[test]
    fn history_section_parses_explicit_retention_days() {
        let toml = r#"
            data_dir = "/tmp/skattr"

            [history]
            retention_days = 90
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.history.retention_days, 90);
    }
}
