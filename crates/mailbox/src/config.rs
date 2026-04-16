// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox server configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Operator-tunable mailbox settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxConfig {
    /// Path to the SQLite store.
    pub storage_path: PathBuf,
    /// Path for Arti's hidden-service state.
    pub arti_state_dir: PathBuf,
    /// Maximum deposit size, bytes. Default 1 MiB.
    #[serde(default = "default_max_deposit_size")]
    pub max_deposit_size: u64,
    /// Maximum TTL a depositor can request, in days. Default 30.
    #[serde(default = "default_max_ttl_days")]
    pub max_ttl_days: u32,
    /// Optional bind-time hostname for structured logs (never
    /// transmitted). Helps operators tell instances apart in logs.
    #[serde(default)]
    pub instance_label: Option<String>,
}

fn default_max_deposit_size() -> u64 {
    1024 * 1024
}

fn default_max_ttl_days() -> u32 {
    30
}

impl MailboxConfig {
    /// Load from a TOML file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Default config for a brand-new install.
    pub fn defaults() -> anyhow::Result<Self> {
        let dirs = directories::ProjectDirs::from("net", "myggiz", "skattr-mailbox")
            .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        let data_dir = dirs.data_dir();
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            storage_path: data_dir.join("mailbox.sqlite"),
            arti_state_dir: data_dir.join("arti"),
            max_deposit_size: default_max_deposit_size(),
            max_ttl_days: default_max_ttl_days(),
            instance_label: None,
        })
    }
}
