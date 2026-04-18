// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Cross-crate integration-test helpers.
//!
//! Intended usage: a test spins up two in-process daemons, waits for
//! Tor bootstrap (or a testnet equivalent), exchanges the full Phase 1
//! handshake, and asserts end-to-end behaviour. For Phase 0 the helpers
//! are stubbed.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(test)]
mod arti_echo;

use std::time::Duration;

use tempfile::TempDir;

/// Handle to a spawned test daemon.
pub struct TestDaemon {
    /// Per-daemon scratch directory. Removed on drop.
    pub data_dir: TempDir,
}

impl TestDaemon {
    /// Spawn a new daemon with a fresh data directory.
    pub async fn spawn() -> anyhow::Result<Self> {
        let data_dir = tempfile::tempdir()?;
        Ok(Self { data_dir })
    }
}

/// Wait for a daemon's Tor runtime to reach [`skattr_core::daemon::TorStatus::Ready`].
pub async fn wait_for_ready(_d: &TestDaemon, _max: Duration) -> anyhow::Result<()> {
    // Placeholder until transport::tor has a real bootstrap implementation.
    Ok(())
}
