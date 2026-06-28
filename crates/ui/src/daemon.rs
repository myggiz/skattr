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
use skattr_core::daemon::logs::LogSink;
use skattr_core::daemon::{Config, Daemon, Ready};

pub struct AppState {
    /// Resolved data directory; set in `tauri::Builder::setup`.
    pub data_dir: RwLock<Option<PathBuf>>,
    /// Passphrase captured by `bootstrap::vault_unlock` /
    /// `bootstrap::identity_init`, consumed by
    /// `start_in_process_cmd`.
    pub pending_passphrase: RwLock<Option<zeroize::Zeroizing<String>>>,
    /// Async-mutex around the post-daemon IPC client. `Some` only
    /// after `start_in_process_cmd` succeeds.
    pub ipc: Mutex<Option<IpcClient<skattr_core::daemon::ipc::IpcStream>>>,
    /// Cached `Ready` snapshot from `Daemon::run`.
    pub ready: RwLock<Option<Ready>>,
    /// Daemon task handle; held so shutdown can `abort` if needed.
    pub task: Mutex<Option<JoinHandle<skattr_core::error::Result<()>>>>,
    /// Sender for graceful daemon shutdown.
    pub shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Log sink wired to the tracing subscriber. Passed to `Daemon::run`
    /// so tracing events flow into the ring buffer and onto the event bus.
    pub log_sink: LogSink,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            data_dir: RwLock::new(None),
            pending_passphrase: RwLock::new(None),
            ipc: Mutex::new(None),
            ready: RwLock::new(None),
            task: Mutex::new(None),
            shutdown_tx: Mutex::new(None),
            log_sink: LogSink::new(),
        }
    }
}

/// True once the in-process daemon has started and signalled ready this
/// session. Used by the main page to distinguish "vault exists AND we have
/// already unlocked + started the daemon this session" from "vault exists but
/// this is a fresh launch that still needs to unlock". Without this, a relaunch
/// with an existing vault renders the main shell with no running daemon.
#[tauri::command]
pub async fn daemon_running(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.ready.read().is_some())
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

    // Config lives inside the consolidated data dir. Load the persisted file so
    // user settings (download dir, retention, timeouts, …) survive a restart;
    // fall back to defaults on first run.
    let config_path = data_dir.join("config.toml");
    let mut config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(_) => Config::defaults().map_err(|e| format!("config: {e}"))?,
    };
    config.data_dir = data_dir.clone();
    // Pin IPC socket to the well-known path so the CLI keeps working.
    config.ipc_socket = Some(data_dir.join(skattr_core::daemon::ipc::ENDPOINT_FILENAME));

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Ready>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let pass = passphrase.clone();
    let dd = data_dir.clone();
    let log_sink = state.log_sink.clone();
    let task = tokio::spawn(async move {
        Daemon::run_with_sink(
            &dd,
            &pass,
            config,
            config_path,
            ready_tx,
            shutdown_fut,
            Some(log_sink),
        )
        .await
    });

    let ready = match tokio::time::timeout(std::time::Duration::from_secs(180), ready_rx).await {
        // Outer timeout elapsed: the daemon is still alive but never signalled
        // ready within the window.
        Err(_) => return Err("Tor bootstrap timed out (180s)".to_string()),
        Ok(Ok(ready)) => ready,
        // `ready_tx` was dropped without sending — the daemon task ended before
        // it became ready. Await the task to surface its *actual* startup error
        // (e.g. an Arti filesystem-permissions failure) instead of the opaque
        // "ready channel closed early".
        Ok(Err(_recv)) => {
            let msg = match task.await {
                Ok(Ok(())) => "daemon exited before it became ready".to_string(),
                Ok(Err(e)) => format!("daemon failed to start: {e}"),
                Err(join) => format!("daemon task panicked: {join}"),
            };
            return Err(msg);
        }
    };

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
