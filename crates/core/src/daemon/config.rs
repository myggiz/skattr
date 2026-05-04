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

/// Delivery-policy settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryConfig {
    /// How long the hub tries direct connection before falling back to
    /// mailbox deposit. Default 30s (locked in 2.B).
    #[serde(default = "default_direct_timeout_secs")]
    pub direct_timeout_secs: u32,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            direct_timeout_secs: default_direct_timeout_secs(),
        }
    }
}

fn default_direct_timeout_secs() -> u32 {
    30
}

/// Notification settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct NotificationsConfig {
    /// Notification display mode (Full, Minimal, or Silent).
    #[serde(default)]
    pub mode: crate::daemon::commands::NotificationMode,
}

/// UI / shell settings (close-to-tray, start-minimised, log persistence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiConfig {
    /// If true, closing the window hides to tray instead of exiting.
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    /// If true, the app starts minimised to tray.
    #[serde(default)]
    pub start_minimised: bool,
    /// If true, daemon logs are persisted to disk.
    #[serde(default)]
    pub persist_logs_to_disk: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            close_to_tray: default_close_to_tray(),
            start_minimised: false,
            persist_logs_to_disk: false,
        }
    }
}

fn default_close_to_tray() -> bool {
    true
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
    /// Delivery policy. New in 2.F.
    #[serde(default)]
    pub delivery: DeliveryConfig,
    /// Notification settings. New in 2.F.
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// UI / shell settings. New in 2.F.
    #[serde(default)]
    pub ui: UiConfig,
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
            delivery: DeliveryConfig::default(),
            notifications: NotificationsConfig::default(),
            ui: UiConfig::default(),
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
            delivery: DeliveryConfig::default(),
            notifications: NotificationsConfig::default(),
            ui: UiConfig::default(),
        }
    }

    /// Apply a `ConfigPatch`, mutating `self` in place. Returns
    /// `Err(CoreError::Config(...))` if any field fails validation.
    /// Validation rules:
    ///   - `direct_timeout_secs`: 1..=600
    ///   - `history_retention_days`: any u32 (0 = infinite)
    ///   - bool fields: trivially valid
    ///   - `notification_mode`: enum-bounded
    pub fn apply_patch(&mut self, patch: &crate::daemon::commands::ConfigPatch) -> Result<()> {
        if let Some(d) = patch.history_retention_days {
            self.history.retention_days = d;
        }
        if let Some(t) = patch.direct_timeout_secs {
            if !(1..=600).contains(&t) {
                return Err(CoreError::Config(format!(
                    "direct_timeout_secs out of range 1..=600 (got {t})"
                )));
            }
            self.delivery.direct_timeout_secs = t;
        }
        if let Some(m) = patch.notification_mode {
            self.notifications.mode = m;
        }
        if let Some(b) = patch.close_to_tray {
            self.ui.close_to_tray = b;
        }
        if let Some(b) = patch.start_minimised {
            self.ui.start_minimised = b;
        }
        if let Some(b) = patch.persist_logs_to_disk {
            self.ui.persist_logs_to_disk = b;
        }
        Ok(())
    }

    /// Project onto the wire `ConfigSnapshot` (UI-relevant fields only).
    pub fn snapshot(&self) -> crate::daemon::commands::ConfigSnapshot {
        crate::daemon::commands::ConfigSnapshot {
            history_retention_days: self.history.retention_days,
            direct_timeout_secs: self.delivery.direct_timeout_secs,
            notification_mode: self.notifications.mode,
            close_to_tray: self.ui.close_to_tray,
            start_minimised: self.ui.start_minimised,
            persist_logs_to_disk: self.ui.persist_logs_to_disk,
        }
    }

    /// Atomically write `self` to a TOML file at `path`. Writes to
    /// `<path>.tmp`, fsyncs, renames, then fsyncs the parent dir. Mode
    /// 0600 on Unix.
    pub fn save_to_disk(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let serialised = toml::to_string_pretty(self)
            .map_err(|e| CoreError::Config(format!("serialise: {e}")))?;

        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .map_err(|e| CoreError::Config(format!("create {}: {e}", tmp.display())))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = f
                    .metadata()
                    .map_err(|e| CoreError::Config(format!("stat: {e}")))?
                    .permissions();
                perm.set_mode(0o600);
                f.set_permissions(perm)
                    .map_err(|e| CoreError::Config(format!("chmod: {e}")))?;
            }
            f.write_all(serialised.as_bytes())
                .map_err(|e| CoreError::Config(format!("write: {e}")))?;
            f.sync_all()
                .map_err(|e| CoreError::Config(format!("fsync: {e}")))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| {
            CoreError::Config(format!(
                "rename {} → {}: {e}",
                tmp.display(),
                path.display()
            ))
        })?;
        if let Some(parent) = path.parent() {
            // Best-effort directory fsync.
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
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

    #[test]
    fn old_config_without_2f_sections_still_parses() {
        let toml = r#"
            data_dir = "/tmp/skattr"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delivery.direct_timeout_secs, 30);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Full
        ));
        assert!(cfg.ui.close_to_tray);
        assert!(!cfg.ui.start_minimised);
        assert!(!cfg.ui.persist_logs_to_disk);
    }

    #[test]
    fn explicit_2f_sections_parse() {
        let toml = r#"
            data_dir = "/tmp/skattr"

            [delivery]
            direct_timeout_secs = 45

            [notifications]
            mode = "minimal"

            [ui]
            close_to_tray = false
            start_minimised = true
            persist_logs_to_disk = true
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delivery.direct_timeout_secs, 45);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Minimal
        ));
        assert!(!cfg.ui.close_to_tray);
        assert!(cfg.ui.start_minimised);
        assert!(cfg.ui.persist_logs_to_disk);
    }

    #[test]
    fn apply_patch_partial_only_touches_specified_fields() {
        let mut cfg = Config::defaults().unwrap();
        cfg.history.retention_days = 7;
        let patch = crate::daemon::commands::ConfigPatch {
            notification_mode: Some(crate::daemon::commands::NotificationMode::Off),
            ..Default::default()
        };
        cfg.apply_patch(&patch).unwrap();
        assert_eq!(cfg.history.retention_days, 7);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Off
        ));
    }

    #[test]
    fn apply_patch_rejects_out_of_range_timeout() {
        let mut cfg = Config::defaults().unwrap();
        let patch = crate::daemon::commands::ConfigPatch {
            direct_timeout_secs: Some(0),
            ..Default::default()
        };
        assert!(cfg.apply_patch(&patch).is_err());

        let patch = crate::daemon::commands::ConfigPatch {
            direct_timeout_secs: Some(601),
            ..Default::default()
        };
        assert!(cfg.apply_patch(&patch).is_err());
    }

    #[test]
    fn save_to_disk_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let mut cfg = Config::defaults().unwrap();
        cfg.delivery.direct_timeout_secs = 45;
        cfg.save_to_disk(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.delivery.direct_timeout_secs, 45);
    }

    #[test]
    fn snapshot_projects_all_fields() {
        let cfg = Config::defaults().unwrap();
        let snap = cfg.snapshot();
        assert_eq!(snap.history_retention_days, 0);
        assert_eq!(snap.direct_timeout_secs, 30);
        assert!(snap.close_to_tray);
        assert!(!snap.start_minimised);
        assert!(!snap.persist_logs_to_disk);
    }
}
