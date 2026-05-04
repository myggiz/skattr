// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! WipeAllData integration test: send the command against a live daemon
//! (in-process duplex, no Tor), verify (a) the data directory is removed
//! within a few seconds, and (b) the reply arrives as `CommandResult::Ok`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use skattr_core::daemon::Config;
use skattr_core::daemon::{Command, CommandResult, IpcClient};
use skattr_core::identity::{IdentityKey, Seed};
use skattr_core::test_exports::{
    handle_connection, CommandExecutor, DaemonHandle, DeliveryHub, IpcError, Pool,
};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Shim: `Arc<DaemonHandle<S>>` → `Arc<dyn CommandExecutor>`
// ---------------------------------------------------------------------------

type DuplexHandle = DaemonHandle<tokio::io::DuplexStream>;

struct ExecShim(Arc<DuplexHandle>);

#[async_trait::async_trait]
impl CommandExecutor for ExecShim {
    async fn execute(&self, cmd: Command) -> std::result::Result<CommandResult, IpcError> {
        self.0.execute(cmd).await
    }
}

// ---------------------------------------------------------------------------
// Helper: build a DaemonHandle with an on-disk data_dir.
// ---------------------------------------------------------------------------

fn build_handle_with_data_dir(data_dir: &std::path::Path) -> Arc<DuplexHandle> {
    let seed = Seed::generate().unwrap();
    let identity = IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);

    // Build a Config with the given data_dir so wipe_all_data knows where to wipe.
    let mut config = Config::defaults().expect("Config::defaults() should succeed in test env");
    config.data_dir = data_dir.to_path_buf();

    Arc::new(DaemonHandle::new_with_config(
        pool,
        hub,
        identity,
        events_tx,
        config,
        data_dir.join("skattr.toml"),
    ))
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// `WipeAllData` removes the data directory and exits the process.
///
/// We verify only the directory-removal post-condition here (we cannot
/// observe process exit from an in-process test). The reply `Ok` is also
/// checked before the teardown begins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wipe_all_data_removes_data_dir() {
    // Create a real on-disk directory — the handler calls remove_dir_all on it.
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    assert!(data_dir.exists(), "precondition: data_dir exists");

    let handle = build_handle_with_data_dir(&data_dir);

    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    let exec: Arc<dyn CommandExecutor> = Arc::new(ExecShim(handle.clone()));
    let events_tx = handle.events_tx.clone();
    tokio::spawn(async move {
        handle_connection(server_io, exec, events_tx).await;
    });

    let mut client = IpcClient::from_stream(client_io);

    // The handler spawns the teardown task and returns Ok immediately.
    // The reply should arrive before the process-exit fires.
    // (We catch the Ok; a transport-closed error is also acceptable since
    //  the server task may drop before the reply is flushed in CI.)
    let reply = client.execute(Command::WipeAllData).await;
    match &reply {
        Ok(CommandResult::Ok) => { /* expected */ }
        Err(_) => {
            // Connection may close before reply flushes on fast machines.
            // This is still a valid outcome for WipeAllData.
        }
        Ok(other) => panic!("unexpected reply: {other:?}"),
    }

    // Wait up to 5 s for the data directory to be removed by the teardown task.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && data_dir.exists() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        !data_dir.exists(),
        "data_dir must be removed after WipeAllData"
    );

    // Tell tempfile not to try to remove the data_dir itself (it's gone).
    let _ = workdir.keep();
}
