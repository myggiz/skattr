// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Phase 2.C: end-to-end first-run integration. Bootstraps a real
//! Tor-backed daemon via `Daemon::run`, drives `Command::DaemonInfo`
//! and `Command::ListContacts` over the IPC socket, and asserts the
//! Subscribe TorStatus replay surfaces. `#[ignore]`-gated; run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored ui_first_run
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::{Command, CommandResult, Config, Daemon, IpcClient, TorStatus};
use skattr_core::identity::{IdentityKey, Seed, Vault};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// First-run smoke test: boots a real Tor daemon, drives DaemonInfo,
/// subscribes to TorStatus and asserts the cached Ready replay, and
/// asserts ListContacts returns [] for a fresh vault.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns real Arti; run with: cargo test -p skattr-tests --release -- --ignored ui_first_run"]
async fn ui_first_run_daemon_info_and_subscribe_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let pw = Zeroizing::new("ui-first-run-passphrase".to_string());

    // Pre-stage a vault as if the wizard's identity_init step had run.
    let seed = Seed::generate().unwrap();
    let key = IdentityKey::from_seed(&seed).unwrap();
    Vault::create(&data_dir.join("identity.vault"), key, pw.as_str()).unwrap();

    let mut config = Config::defaults().unwrap();
    config.data_dir = data_dir.clone();
    config.ipc_socket = Some(data_dir.join("ipc.sock"));

    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let data_dir_owned = data_dir.clone();
    let pw_for_run = pw.clone();
    let config_owned = config.clone();
    let task = tokio::spawn(async move {
        Daemon::run(
            &data_dir_owned,
            &pw_for_run,
            config_owned,
            std::path::PathBuf::from("/dev/null"),
            ready_tx,
            shutdown_fut,
        )
        .await
    });

    let ready = tokio::time::timeout(Duration::from_secs(180), ready_rx)
        .await
        .expect("daemon bootstraps within 180s")
        .expect("ready_tx still open");

    // --- DaemonInfo ---
    let mut req_client = IpcClient::connect(&ready.ipc_socket).await.unwrap();
    let info = req_client.execute(Command::DaemonInfo).await.unwrap();
    match info {
        CommandResult::DaemonInfo {
            current_onion,
            daemon_version,
            schema_version,
            ..
        } => {
            assert_eq!(
                current_onion.as_deref(),
                Some(ready.onion.as_str()),
                "DaemonInfo onion must match ready onion"
            );
            assert_eq!(
                daemon_version,
                env!("CARGO_PKG_VERSION"),
                "daemon_version must equal crate version"
            );
            assert!(
                schema_version >= 9,
                "schema_version must be at least 9, got {schema_version}"
            );
        }
        other => panic!("expected DaemonInfo, got {other:?}"),
    }

    // --- TorStatus subscribe: expect cached Ready replay within 5s ---
    let mut sub_client = IpcClient::connect(&ready.ipc_socket).await.unwrap();
    sub_client.subscribe(EventFilter::TorStatus).await.unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), sub_client.next_event())
        .await
        .expect("TorStatusChanged(Ready) replay within 5s")
        .expect("event read ok");
    assert!(
        matches!(
            ev,
            skattr_core::daemon::Event::TorStatusChanged(TorStatus::Ready)
        ),
        "expected TorStatusChanged(Ready), got {ev:?}"
    );

    // --- ListContacts: fresh vault must return [] ---
    let lc = req_client.execute(Command::ListContacts).await.unwrap();
    assert!(
        matches!(lc, CommandResult::Contacts(ref v) if v.is_empty()),
        "expected empty contact list for fresh vault, got {lc:?}"
    );

    // --- Graceful shutdown ---
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(30), task).await;
}
