// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Post-daemon Tauri command: `ipc_request`. Single generic command
//! that proxies any `IpcRequest` to the daemon and returns the wire
//! response verbatim.

use skattr_core::daemon::commands::Command;
use skattr_core::daemon::ipc::wire::{IpcError, IpcResponse};
use skattr_core::daemon::ipc::IpcClient;

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
    // Open a FRESH connection per request. The daemon's IPC server treats a
    // non-subscribed connection as one-shot: it closes after a single Execute
    // (see ipc/server: `is_terminal = subscribed.is_none()`). Reusing one
    // persistent IpcClient therefore breaks on the 2nd request with a broken
    // pipe. A new connection per request matches that design (events.rs does
    // the same for subscriptions).
    let socket_path = state
        .ready
        .read()
        .clone()
        .ok_or_else(|| "daemon not yet running; call start_in_process_cmd first".to_string())?
        .ipc_socket;
    let mut client = IpcClient::connect(&socket_path)
        .await
        .map_err(|e| format!("ipc connect: {e}"))?;
    match client.execute(cmd).await {
        Ok(result) => Ok(IpcResponse::Ok(result)),
        Err(e) => Ok(IpcResponse::Err(map_client_err(e))),
    }
}

/// Preserve the daemon's structured `IpcError` instead of flattening it.
/// `IpcClientError::Server` already carries the typed wire error the daemon
/// produced (via `CoreError::kind()`); only genuine transport/codec failures
/// become `Internal`.
fn map_client_err(e: skattr_core::daemon::ipc::IpcClientError) -> IpcError {
    use skattr_core::daemon::ipc::IpcClientError;
    match e {
        IpcClientError::Server(ipc_err) => ipc_err,
        other => {
            let msg: String = format!("{other}").chars().take(256).collect();
            IpcError::Internal(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_error_passes_through_structured() {
        use skattr_core::daemon::error_kind::DaemonErrorKind;
        use skattr_core::daemon::ipc::IpcClientError;
        let e = IpcClientError::Server(IpcError::Daemon(DaemonErrorKind::InviteExpired));
        assert!(
            matches!(
                map_client_err(e),
                IpcError::Daemon(DaemonErrorKind::InviteExpired)
            ),
            "expected structured Daemon(InviteExpired)"
        );
    }

    #[test]
    fn transport_error_becomes_internal() {
        use skattr_core::daemon::ipc::IpcClientError;
        let e = IpcClientError::DaemonNotRunning;
        assert!(matches!(map_client_err(e), IpcError::Internal(_)));
    }
}
