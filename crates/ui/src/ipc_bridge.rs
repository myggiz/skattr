// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Post-daemon Tauri command: `ipc_request`. Single generic command
//! that proxies any `IpcRequest` to the daemon over the in-process
//! `IpcClient` and returns the wire response verbatim.

use skattr_core::daemon::commands::Command;
use skattr_core::daemon::ipc::wire::{IpcError, IpcResponse};

use crate::daemon::AppState;

#[tauri::command]
pub async fn ipc_request(
    state: tauri::State<'_, AppState>,
    cmd: Command,
) -> Result<IpcResponse, String> {
    let mut guard = state.ipc.lock().await;
    let client = guard
        .as_mut()
        .ok_or_else(|| "daemon not yet running; call start_in_process_cmd first".to_string())?;
    match client.execute(cmd).await {
        Ok(result) => Ok(IpcResponse::Ok(result)),
        Err(e) => Ok(IpcResponse::Err(IpcError::Internal(format!("{e}")))),
    }
}
