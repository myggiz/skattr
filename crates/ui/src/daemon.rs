// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! In-process `Daemon::run` lifecycle. `start_in_process_cmd` is the
//! Tauri command the wizard's final step calls after `vault_unlock`
//! has stashed the passphrase. The command spawns `Daemon::run` on a
//! Tokio task, opens an `IpcClient` against the returned socket path,
//! and parks both in `AppState` for the post-daemon command surface
//! to use.

use std::path::PathBuf;

use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use skattr_core::daemon::ipc::IpcClient;
use skattr_core::daemon::{Config, Daemon, Ready};

#[derive(Default)]
pub struct AppState {
    /// Resolved data directory; set in `tauri::Builder::setup`.
    pub data_dir: RwLock<Option<PathBuf>>,
    /// Passphrase captured by `bootstrap::vault_unlock` /
    /// `bootstrap::identity_init`, consumed by
    /// `start_in_process_cmd`.
    pub pending_passphrase: RwLock<Option<zeroize::Zeroizing<String>>>,
    /// Async-mutex around the post-daemon IPC client. `Some` only
    /// after `start_in_process_cmd` succeeds.
    pub ipc: Mutex<Option<IpcClient<tokio::net::UnixStream>>>,
    /// Cached `Ready` snapshot from `Daemon::run`.
    pub ready: RwLock<Option<Ready>>,
    /// Daemon task handle; held so shutdown can `abort` if needed.
    pub task: Mutex<Option<JoinHandle<skattr_core::error::Result<()>>>>,
    /// Sender for graceful daemon shutdown.
    pub shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[tauri::command]
pub async fn start_in_process_cmd(state: tauri::State<'_, AppState>) -> Result<Ready, String> {
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let passphrase = state.pending_passphrase.write().take().ok_or_else(|| {
        "no pending passphrase; call vault_unlock or identity_init first".to_string()
    })?;

    let mut config = Config::defaults().map_err(|e| format!("config: {e}"))?;
    config.data_dir = data_dir.clone();
    // Pin IPC socket to the well-known path so the CLI keeps working.
    config.ipc_socket = Some(data_dir.join("ipc.sock"));

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Ready>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let pass = passphrase.clone();
    let dd = data_dir.clone();
    let config_path = skattr_core::daemon::config::resolve_config_path(None);
    let task = tokio::spawn(async move {
        Daemon::run(&dd, &pass, config, config_path, ready_tx, shutdown_fut).await
    });

    let ready = tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx)
        .await
        .map_err(|_| "Tor bootstrap timed out (180s)".to_string())?
        .map_err(|_| "ready channel closed early".to_string())?;

    let client = IpcClient::connect(&ready.ipc_socket)
        .await
        .map_err(|e| format!("ipc connect: {e}"))?;

    *state.ready.write() = Some(ready.clone());
    *state.shutdown_tx.lock().await = Some(shutdown_tx);
    *state.task.lock().await = Some(task);
    *state.ipc.lock().await = Some(client);

    Ok(ready)
}

/// Graceful shutdown — drains the daemon over the shutdown oneshot,
/// joins the task with a timeout. Called from the close-window hook.
pub async fn shutdown(app: &tauri::AppHandle) {
    let state = tauri::Manager::state::<AppState>(app);
    let tx = state.shutdown_tx.lock().await.take();
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
    let handle = state.task.lock().await.take();
    if let Some(handle) = handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
    }
}
