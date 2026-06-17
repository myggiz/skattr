// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Settings round-trip: GetConfig → SetConfig → GetConfig.
//!
//! Exercises Plan Tasks 12 + 4 + 8 + 9 end-to-end through a real
//! daemon dispatch loop, including `apply_patch` validation and
//! the persisted state surviving a simulated daemon restart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use skattr_core::daemon::commands::{ConfigPatch, NotificationMode};
use skattr_core::daemon::{Command, CommandResult, Config, DaemonErrorKind};
use skattr_core::identity::{IdentityKey, Seed};
use skattr_core::test_exports::{CommandExecutor, DaemonHandle, DeliveryHub, IpcError, Pool};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Helper: build a DaemonHandle with given config and config_path.
// ---------------------------------------------------------------------------

type DuplexHandle = DaemonHandle<tokio::io::DuplexStream>;

fn build_handle(config: Config, config_path: std::path::PathBuf) -> Arc<DuplexHandle> {
    let seed = Seed::generate().unwrap();
    let identity = IdentityKey::from_seed(&seed).unwrap();
    let pool = Arc::new(Pool::in_memory());
    let hub: Arc<DeliveryHub<tokio::io::DuplexStream>> = Arc::new(DeliveryHub::new(pool.clone()));
    let (events_tx, _) = broadcast::channel(16);
    Arc::new(DaemonHandle::new_with_config(
        pool,
        hub,
        identity,
        events_tx,
        config,
        config_path,
    ))
}

async fn send(handle: &Arc<DuplexHandle>, cmd: Command) -> Result<CommandResult, IpcError> {
    handle.execute(cmd).await
}

// ---------------------------------------------------------------------------
// Test 1: defaults → full patch → round-trip → restart → persistence.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_round_trip_persists_and_reloads() {
    let workdir = tempfile::tempdir().unwrap();
    let config_path = workdir.path().join("skattr.toml");

    // ── Boot 1: build handle with defaults, assert defaults. ──
    let mut cfg = Config::defaults().expect("Config::defaults() must succeed in test env");
    cfg.data_dir = workdir.path().join("data");
    let handle = build_handle(cfg, config_path.clone());

    let snap = match send(&handle, Command::GetConfig).await.unwrap() {
        CommandResult::Config(s) => s,
        other => panic!("expected Config, got {other:?}"),
    };
    assert_eq!(snap.history_retention_days, 0, "default retention_days");
    assert_eq!(snap.direct_timeout_secs, 30, "default direct_timeout_secs");
    assert!(
        matches!(snap.notification_mode, NotificationMode::Full),
        "default notification_mode"
    );
    assert!(snap.close_to_tray, "default close_to_tray is true");
    assert!(!snap.start_minimised, "default start_minimised");
    assert!(!snap.persist_logs_to_disk, "default persist_logs_to_disk");

    // ── Apply full patch. ──
    let patch = ConfigPatch {
        history_retention_days: Some(30),
        direct_timeout_secs: Some(60),
        notification_mode: Some(NotificationMode::Minimal),
        close_to_tray: Some(false),
        start_minimised: Some(true),
        persist_logs_to_disk: Some(true),
        download_dir: None,
    };
    let result = send(&handle, Command::SetConfig { patch }).await.unwrap();
    assert!(
        matches!(result, CommandResult::Ok),
        "SetConfig must return Ok"
    );

    // ── Round-trip within same boot. ──
    let snap = match send(&handle, Command::GetConfig).await.unwrap() {
        CommandResult::Config(s) => s,
        other => panic!("expected Config, got {other:?}"),
    };
    assert_eq!(snap.history_retention_days, 30);
    assert_eq!(snap.direct_timeout_secs, 60);
    assert!(matches!(snap.notification_mode, NotificationMode::Minimal));
    assert!(!snap.close_to_tray);
    assert!(snap.start_minimised);
    assert!(snap.persist_logs_to_disk);

    // ── Drop boot-1 handle. ──
    drop(handle);

    // ── Boot 2: reload config from disk, assert persistence. ──
    assert!(
        config_path.exists(),
        "config.toml must exist after SetConfig save"
    );
    let cfg2 =
        Config::load_with_precedence(Some(&config_path), None, None, None).expect("load config");
    let handle2 = build_handle(cfg2, config_path.clone());

    let snap2 = match send(&handle2, Command::GetConfig).await.unwrap() {
        CommandResult::Config(s) => s,
        other => panic!("expected Config on boot-2, got {other:?}"),
    };
    assert_eq!(snap2.history_retention_days, 30, "persisted retention_days");
    assert_eq!(
        snap2.direct_timeout_secs, 60,
        "persisted direct_timeout_secs"
    );
    assert!(
        matches!(snap2.notification_mode, NotificationMode::Minimal),
        "persisted notification_mode"
    );
    assert!(!snap2.close_to_tray, "persisted close_to_tray");
    assert!(snap2.start_minimised, "persisted start_minimised");
    assert!(snap2.persist_logs_to_disk, "persisted persist_logs_to_disk");
}

// ---------------------------------------------------------------------------
// Test 2: out-of-range direct_timeout_secs → InvalidArgument.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_config_validates_direct_timeout_range() {
    let workdir = tempfile::tempdir().unwrap();
    let config_path = workdir.path().join("skattr.toml");

    let mut cfg = Config::defaults().expect("Config::defaults() must succeed in test env");
    cfg.data_dir = workdir.path().join("data");
    let handle = build_handle(cfg, config_path);

    // direct_timeout_secs = 0 is below the valid range of 1..=600.
    let patch = ConfigPatch {
        direct_timeout_secs: Some(0),
        ..ConfigPatch::default()
    };
    let err = send(&handle, Command::SetConfig { patch })
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            IpcError::Daemon(DaemonErrorKind::InvalidArgument { .. })
        ),
        "expected InvalidArgument, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: partial patch leaves unspecified fields unchanged.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_config_partial_patch_leaves_other_fields_unchanged() {
    let workdir = tempfile::tempdir().unwrap();
    let config_path = workdir.path().join("skattr.toml");

    let mut cfg = Config::defaults().expect("Config::defaults() must succeed in test env");
    cfg.data_dir = workdir.path().join("data");
    let handle = build_handle(cfg, config_path);

    // Only set retention_days; direct_timeout_secs must stay at default (30).
    let patch = ConfigPatch {
        history_retention_days: Some(7),
        ..ConfigPatch::default()
    };
    send(&handle, Command::SetConfig { patch }).await.unwrap();

    let snap = match send(&handle, Command::GetConfig).await.unwrap() {
        CommandResult::Config(s) => s,
        other => panic!("expected Config, got {other:?}"),
    };
    assert_eq!(snap.history_retention_days, 7);
    assert_eq!(
        snap.direct_timeout_secs, 30,
        "unpatched field must retain default"
    );
}
