// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Phase 2.B exit criterion: a cleanly shut-down daemon leaves no plaintext
//! storage DB (or WAL/SHM/sentinel) on disk, only a decryptable `.age`.
//!
//! This drives the REAL `run_with_transport` assembly via the
//! `test_exports::run_loopback` entrypoint (an exact twin of
//! `Daemon::run_with_sink` that swaps Arti for an in-process
//! `LoopbackTransport`). A single daemon is booted, awaited ready, then
//! cleanly shut down by firing its shutdown oneshot and AWAITING the run
//! future to return `Ok(())`. Awaiting the run future is what makes this
//! deterministic: it cannot return until the Task 3 teardown
//! `pool.close()` has run, so the on-disk assertions race nothing.
//!
//! The decrypt-round-trip is intentionally OMITTED: `storage::Pool` and the
//! storage `Seed` are `pub(crate)` and not reachable from `crates/tests`, so
//! reconstructing them here would be fragile. The file-level assertions
//! below alone prove the exit criterion, and the `Pool` decrypt round-trips
//! are already covered by the unit tests added in Tasks 1/4
//! (`crates/core/src/storage/pool.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use crate::loopback_harness::{config_for, init_vault, PASSPHRASE};
use skattr_core::test_exports::{run_loopback, LoopbackNet};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// Boot one real daemon assembly over `LoopbackTransport`, cleanly shut it
/// down, and assert that no plaintext storage DB (or WAL/SHM/sentinel)
/// survives — only a non-empty encrypted `.age`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_shutdown_leaves_only_encrypted_db() {
    let tmp = tempfile::tempdir().unwrap();
    init_vault(tmp.path());

    let net = LoopbackNet::new();
    let pw = Zeroizing::new(PASSPHRASE.to_string());
    let cfg = config_for(tmp.path());

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let data_dir = tmp.path().to_path_buf();
    let task = tokio::spawn(async move {
        run_loopback(
            &data_dir,
            &pw,
            cfg,
            std::path::PathBuf::from("/dev/null"),
            net,
            "solo.onion".into(),
            ready_tx,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    // Await readiness so the daemon has fully opened (and migrated) its Pool.
    let _ready = tokio::time::timeout(Duration::from_secs(60), ready_rx)
        .await
        .expect("daemon ready within 60 s")
        .expect("ready_tx still open");

    // While the daemon is running, the plaintext working DB + sentinel exist.
    // Assert this first so a regression that never opens the plaintext DB (and
    // thus trivially leaves none behind) is caught here rather than passing the
    // post-shutdown assertions vacuously.
    let dd = tmp.path();
    assert!(
        dd.join("skattr.sqlite").exists(),
        "plaintext DB must exist while the daemon is running"
    );
    assert!(
        dd.join("skattr.sqlite.open").exists(),
        "sentinel must exist while the daemon holds the Pool open"
    );

    // Clean shutdown: fire the shutdown future, then AWAIT the run future. The
    // run future cannot return until the Task 3 teardown `pool.close()` has
    // checkpointed + encrypted + removed the plaintext artifacts, so the
    // assertions below are deterministic (no fixed sleeps, no races).
    let _ = shutdown_tx.send(());
    let run_result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("run future returns within 30 s of shutdown")
        .expect("run task did not panic");
    run_result.expect("run_loopback returned Ok(()) on clean shutdown");

    // CORE assertions: clean shutdown leaves only a decryptable `.age`.
    assert!(
        !dd.join("skattr.sqlite").exists(),
        "plaintext DB must be gone after clean shutdown"
    );
    assert!(
        !dd.join("skattr.sqlite-wal").exists(),
        "WAL sidecar must be gone"
    );
    assert!(
        !dd.join("skattr.sqlite-shm").exists(),
        "SHM sidecar must be gone"
    );
    assert!(
        !dd.join("skattr.sqlite.open").exists(),
        "sentinel must be gone"
    );
    let age = dd.join("skattr.sqlite.age");
    assert!(age.exists(), "encrypted DB must exist after clean shutdown");
    assert!(
        std::fs::metadata(&age).unwrap().len() > 0,
        ".age must be non-empty"
    );
}
