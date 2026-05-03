// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Post-daemon Tauri command: `ipc_request`. Single generic command
//! that proxies any `IpcRequest` to the daemon over the in-process
//! `IpcClient` and returns the wire response verbatim.

use skattr_core::daemon::commands::Command;
use skattr_core::daemon::ipc::wire::{IpcError, IpcResponse};

use crate::daemon::AppState;

/// Render an invite link to SVG markup. Pre-daemon-friendly — does
/// not touch `AppState`. Used by `InviteGenerateDialog`.
#[tauri::command]
pub async fn render_invite_qr(url: String) -> Result<String, String> {
    use skattr_core::invite::InviteLink;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| format!("clock: {e}"))?;
    let link = InviteLink::from_url(&url, now).map_err(|e| format!("parse invite: {e}"))?;
    skattr_core::invite::qr::render_svg(&link).map_err(|e| format!("render qr: {e}"))
}

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
