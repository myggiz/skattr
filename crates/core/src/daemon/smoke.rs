// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Smoke-test entry point for release artefacts.
//!
//! `run_smoke` initialises a throwaway vault, boots the daemon, waits
//! for `TorStatus::Ready`, then triggers a graceful shutdown. Used by
//! the `skattr-ui --smoke-test` argv branch in CI release pipelines
//! to verify the bundled binary actually starts on each platform.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Configuration for [`run_smoke`].
#[derive(Debug, Clone)]
pub struct SmokeConfig {
    /// Empty or non-existent directory the smoke owns. The smoke
    /// refuses to run if any user state is present.
    pub data_dir: PathBuf,
    /// Maximum time to wait for `TorStatus::Ready` before failing.
    pub tor_ready_timeout: Duration,
    /// Override the throwaway seed entropy. `[0u8; 32]` (the default)
    /// means "generate from `OsRng`".
    pub seed_bytes: [u8; 32],
}

impl Default for SmokeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::new(),
            tor_ready_timeout: Duration::from_secs(240),
            seed_bytes: [0u8; 32],
        }
    }
}

/// Result emitted on a successful smoke run.
#[derive(Debug, Clone)]
pub struct SmokeReport {
    /// The published v3 onion address.
    pub onion: String,
    /// Wall-clock time from `run_smoke` start to `Ready`.
    pub duration: Duration,
}

/// Smoke-test failure modes.
#[derive(Debug, Error)]
pub enum SmokeError {
    /// `data_dir` already contains user state; refuse to clobber.
    #[error("smoke: data_dir not empty (found {found})")]
    DataDirNotEmpty {
        /// Description of the offending entry (e.g. "identity.vault").
        found: String,
    },
    /// Vault creation failed.
    #[error("smoke: vault create: {0}")]
    VaultCreate(String),
    /// Daemon failed to start.
    #[error("smoke: daemon start: {0}")]
    DaemonStart(String),
    /// `TorStatus::Ready` did not arrive within the configured timeout.
    #[error("smoke: tor bootstrap timed out after {waited:?}")]
    TorTimeout {
        /// Elapsed time before the timeout fired.
        waited: Duration,
    },
    /// Something else went wrong (I/O, channel close, etc.).
    #[error("smoke: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_error_displays_data_dir_not_empty() {
        let e = SmokeError::DataDirNotEmpty {
            found: "identity.vault".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("data_dir not empty"));
        assert!(s.contains("identity.vault"));
    }

    #[test]
    fn smoke_error_displays_tor_timeout() {
        let e = SmokeError::TorTimeout {
            waited: Duration::from_secs(240),
        };
        let s = format!("{e}");
        assert!(s.contains("tor bootstrap timed out"));
    }

    #[test]
    fn smoke_config_default_uses_240s_timeout() {
        let c = SmokeConfig::default();
        assert_eq!(c.tor_ready_timeout, Duration::from_secs(240));
        assert_eq!(c.seed_bytes, [0u8; 32]);
    }
}
