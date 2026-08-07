// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Mailbox server configuration.
//!
//! Loaded from `mailbox.toml`. Layout:
//!
//! ```toml
//! [server]
//! data_dir = "/var/lib/skattr-mailbox"
//! # storage_path / arti_state_dir / health_socket default to
//! # children of data_dir; each can be overridden individually.
//! instance_label = "mailbox-1"
//!
//! [policy]
//! max_deposit_size           = 1048576
//! min_ttl_secs               = 3600
//! max_ttl_secs               = 2592000
//! default_ttl_secs           = 604800
//! recipient_cap_bytes        = 268435456
//! per_conn_deposits_per_min  = 30
//! per_conn_fetches_per_min   = 6
//! global_deposits_per_min    = 1000
//! global_storage_cap_bytes   = 4294967296  # 4 GiB
//! max_recipients             = 100000
//! idle_timeout_secs          = 120
//! max_connections            = 512
//! max_delete_ids             = 1024
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ConfigErrorKind, MailboxError};
use crate::policy::Policy;

/// Top-level mailbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxConfig {
    /// `[server]` section.
    pub server: ServerConfig,
    /// `[policy]` section.
    #[serde(default = "Policy::recommended")]
    pub policy: Policy,
}

/// Filesystem + identity-of-instance settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Parent directory for all server state.
    pub data_dir: PathBuf,
    /// Override `data_dir/mailbox.sqlite`.
    #[serde(default)]
    pub storage_path: Option<PathBuf>,
    /// Override `data_dir/arti`.
    #[serde(default)]
    pub arti_state_dir: Option<PathBuf>,
    /// Override `data_dir/health.sock`.
    #[serde(default)]
    pub health_socket: Option<PathBuf>,
    /// Tag for log disambiguation; never transmitted on the wire.
    #[serde(default)]
    pub instance_label: Option<String>,
}

impl ServerConfig {
    /// Resolved storage path (default: `data_dir/mailbox.sqlite`).
    #[must_use]
    pub fn resolved_storage_path(&self) -> PathBuf {
        self.storage_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("mailbox.sqlite"))
    }
    /// Resolved Arti state dir (default: `data_dir/arti`).
    #[must_use]
    pub fn resolved_arti_state_dir(&self) -> PathBuf {
        self.arti_state_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("arti"))
    }
    /// Resolved healthcheck socket (default: `data_dir/health.sock`).
    #[must_use]
    pub fn resolved_health_socket(&self) -> PathBuf {
        self.health_socket
            .clone()
            .unwrap_or_else(|| self.data_dir.join("health.sock"))
    }
}

impl MailboxConfig {
    /// Load + validate from a TOML file.
    pub fn load(path: &Path) -> Result<Self, MailboxError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| MailboxError::Config(ConfigErrorKind::Io(e.to_string())))?;
        let cfg: MailboxConfig = toml::from_str(&text)
            .map_err(|e| MailboxError::Config(ConfigErrorKind::Parse(e.to_string())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cheapest defaults usable for tests.
    #[must_use]
    pub fn for_tests(data_dir: PathBuf) -> Self {
        Self {
            server: ServerConfig {
                data_dir,
                storage_path: None,
                arti_state_dir: None,
                health_socket: None,
                instance_label: None,
            },
            policy: Policy::recommended(),
        }
    }

    fn validate(&self) -> Result<(), MailboxError> {
        if self.policy.min_ttl_secs > self.policy.max_ttl_secs {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.min_ttl_secs > policy.max_ttl_secs".into(),
            )));
        }
        if self.policy.default_ttl_secs < self.policy.min_ttl_secs
            || self.policy.default_ttl_secs > self.policy.max_ttl_secs
        {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.default_ttl_secs outside [min_ttl_secs, max_ttl_secs]".into(),
            )));
        }
        if self.policy.max_deposit_size == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_deposit_size must be > 0".into(),
            )));
        }
        if self.policy.recipient_cap_bytes < self.policy.max_deposit_size {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.recipient_cap_bytes < max_deposit_size".into(),
            )));
        }
        if self.policy.global_storage_cap_bytes < self.policy.recipient_cap_bytes {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.global_storage_cap_bytes < recipient_cap_bytes".into(),
            )));
        }
        if self.policy.max_recipients == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_recipients must be >= 1".into(),
            )));
        }
        if self.policy.idle_timeout_secs == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.idle_timeout_secs must be >= 1".into(),
            )));
        }
        if self.policy.max_connections == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_connections must be >= 1".into(),
            )));
        }
        if self.policy.max_delete_ids == 0 {
            return Err(MailboxError::Config(ConfigErrorKind::Invalid(
                "policy.max_delete_ids must be >= 1".into(),
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_temp_toml(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("skattr-mailbox-test-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_valid_config() {
        let path = write_temp_toml(
            "valid",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"
instance_label = "test"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        );
        let cfg = MailboxConfig::load(&path).unwrap();
        assert_eq!(cfg.server.instance_label.as_deref(), Some("test"));
        assert_eq!(cfg.policy.max_deposit_size, 1_048_576);
    }

    #[test]
    fn omitted_policy_uses_recommended() {
        let path = write_temp_toml(
            "default-policy",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"
"#,
        );
        let cfg = MailboxConfig::load(&path).unwrap();
        assert_eq!(cfg.policy, Policy::recommended());
    }

    #[test]
    fn rejects_min_gt_max_ttl() {
        let path = write_temp_toml(
            "bad-ttl",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 100
max_ttl_secs = 50
default_ttl_secs = 75
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
"#,
        );
        let err = MailboxConfig::load(&path).expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Config(ConfigErrorKind::Invalid(_))
        ));
    }

    #[test]
    fn omitted_new_caps_use_recommended_defaults() {
        let path = write_temp_toml(
            "default-new-caps",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"
"#,
        );
        let cfg = MailboxConfig::load(&path).unwrap();
        assert_eq!(cfg.policy.global_storage_cap_bytes, 4_294_967_296);
        assert_eq!(cfg.policy.max_recipients, 100_000);
        assert_eq!(cfg.policy.idle_timeout_secs, 120);
        assert_eq!(cfg.policy.max_connections, 512);
        assert_eq!(cfg.policy.max_delete_ids, 1_024);
    }

    #[test]
    fn rejects_global_cap_below_recipient_cap() {
        let path = write_temp_toml(
            "bad-global-cap",
            r#"
[server]
data_dir = "/tmp/skattr-mailbox"

[policy]
max_deposit_size = 1048576
min_ttl_secs = 3600
max_ttl_secs = 2592000
default_ttl_secs = 604800
recipient_cap_bytes = 268435456
per_conn_deposits_per_min = 30
per_conn_fetches_per_min = 6
global_deposits_per_min = 1000
global_storage_cap_bytes = 1024
max_recipients = 100000
idle_timeout_secs = 120
max_connections = 512
max_delete_ids = 1024
"#,
        );
        let err = MailboxConfig::load(&path).expect_err("must reject");
        assert!(matches!(
            err,
            MailboxError::Config(ConfigErrorKind::Invalid(_))
        ));
    }

    #[test]
    fn resolved_paths_default_to_data_dir_children() {
        let cfg = MailboxConfig::for_tests(PathBuf::from("/var/lib/mailbox"));
        assert_eq!(
            cfg.server.resolved_storage_path(),
            PathBuf::from("/var/lib/mailbox/mailbox.sqlite")
        );
        assert_eq!(
            cfg.server.resolved_arti_state_dir(),
            PathBuf::from("/var/lib/mailbox/arti")
        );
        assert_eq!(
            cfg.server.resolved_health_socket(),
            PathBuf::from("/var/lib/mailbox/health.sock")
        );
    }
}
