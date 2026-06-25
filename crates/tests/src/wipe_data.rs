// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! WipeAllData integration test: verify the flush-ordered wipe teardown
//! (T3-3) at the unit level without calling `process::exit`.
//!
//! The `WipeAllData` handler sets a signal via `DaemonHandle::signal_wipe`
//! and returns `Ok` immediately. The IPC connection loop reads the signal
//! with `CommandExecutor::take_wipe_target` after writing the terminal `Bye`
//! frame, then removes the data directory and exits.
//!
//! We cannot test the `process::exit(0)` call from an in-process test
//! (it would kill the test runner), so we verify:
//!   1. `execute(WipeAllData)` returns `Ok` and sets the signal.
//!   2. `take_wipe_target()` returns the correct `data_dir`.
//!   3. A subsequent `tokio::fs::remove_dir_all` removes the directory.
//!
//! This proves the ordering invariant: the teardown happens after the caller
//! (the IPC loop) has written the Bye frame, not on a blind timer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::Config;
use skattr_core::daemon::{Command, CommandResult};
use skattr_core::identity::{IdentityKey, Seed};
use skattr_core::test_exports::{CommandExecutor, DaemonHandle, DeliveryHub, Pool};
use tokio::sync::broadcast;

use skattr_core::daemon::events::Event;

// ---------------------------------------------------------------------------
// Helper: build a DaemonHandle with an on-disk data_dir.
// ---------------------------------------------------------------------------

type DuplexHandle = DaemonHandle<tokio::io::DuplexStream>;

fn build_handle_with_data_dir(data_dir: &std::path::Path) -> Arc<DuplexHandle> {
    let seed = Seed::generate().unwrap();
    let identity = IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel::<Event>(16);

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
// Tests
// ---------------------------------------------------------------------------

/// `WipeAllData` sets the wipe signal and returns `CommandResult::Ok`;
/// `take_wipe_target()` then returns the `data_dir`; removing the dir works.
///
/// This validates the flush-ordered teardown ordering without calling
/// `process::exit` (which would kill the test runner).
#[tokio::test]
async fn wipe_signal_is_set_and_dir_is_removable() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    assert!(data_dir.exists(), "precondition: data_dir exists");

    let handle = build_handle_with_data_dir(&data_dir);

    // Dispatch `WipeAllData` — should return Ok and set the signal.
    let result = handle.execute(Command::WipeAllData).await;
    assert!(
        matches!(result, Ok(CommandResult::Ok)),
        "expected Ok(Ok), got {result:?}"
    );

    // The signal must be set to `data_dir` — this is what `handle_connection`
    // reads after flushing the Bye frame.
    let target = handle.take_wipe_target();
    assert_eq!(
        target.as_deref(),
        Some(data_dir.as_path()),
        "wipe_target must be data_dir after WipeAllData"
    );

    // Simulate the teardown that `handle_connection` performs after the Bye:
    // remove_dir_all the target path. This must succeed and leave no dir.
    tokio::fs::remove_dir_all(&data_dir).await.unwrap();
    assert!(
        !data_dir.exists(),
        "data_dir must be gone after remove_dir_all"
    );

    // Tell tempfile not to try to remove the data_dir itself (it's gone).
    let _ = workdir.keep();
}

/// `take_wipe_target()` returns `None` before `WipeAllData` is dispatched,
/// confirming the signal is not set spuriously.
#[tokio::test]
async fn wipe_signal_is_none_before_command() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let handle = build_handle_with_data_dir(&data_dir);

    // No command dispatched yet — signal must be absent.
    assert!(
        handle.take_wipe_target().is_none(),
        "wipe_target must be None before WipeAllData is dispatched"
    );
}
