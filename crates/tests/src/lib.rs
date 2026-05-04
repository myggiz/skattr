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
mod add_mailbox_validates;
#[cfg(test)]
mod arti_echo;
#[cfg(test)]
mod cli_export;
#[cfg(test)]
mod cli_ipc_roundtrip;
#[cfg(test)]
mod cli_real_tor;
#[cfg(test)]
mod cli_search;
#[cfg(test)]
mod cli_tail_follow;
#[cfg(test)]
mod cli_two_daemons;
#[cfg(test)]
mod delivery_kill_mid_message;
#[cfg(test)]
mod delivery_real_tor;
#[cfg(test)]
mod history_sweep;
#[cfg(test)]
mod invite_roundtrip;
#[cfg(test)]
mod mailbox_client_real_tor;
#[cfg(test)]
mod mailbox_codec_parity;
#[cfg(test)]
mod mailbox_failover;
#[cfg(test)]
mod mailbox_harness;
#[cfg(test)]
mod mailbox_offline_delivery;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod mailbox_real_tor;
#[cfg(test)]
mod mls_pair;
#[cfg(test)]
mod remove_mailbox_drains;
#[cfg(test)]
mod rotate_onion_during_offline;
#[cfg(test)]
mod settings_roundtrip;
#[cfg(test)]
mod ui_first_run;
#[cfg(test)]
mod ui_send_roundtrip;
#[cfg(test)]
mod welcome_propagation;
#[cfg(test)]
mod wipe_data;

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
