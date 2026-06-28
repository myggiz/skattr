// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Long-lived event subscription: opens a fresh IPC connection (the
//! IpcClient in AppState handles request/response; this opens a
//! separate connection for streaming events) and relays each frame to
//! a Tauri Channel for SvelteKit to consume.

use tauri::Emitter;

use skattr_core::daemon::events::Event;
use skattr_core::daemon::ipc::wire::EventFilter;
use skattr_core::daemon::ipc::IpcClient;

use crate::daemon::AppState;

#[tauri::command]
pub async fn ipc_subscribe(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    filter: EventFilter,
    channel: tauri::ipc::Channel<Event>,
) -> Result<(), String> {
    let socket_path = state
        .ready
        .read()
        .clone()
        .ok_or_else(|| "daemon not yet running".to_string())?
        .ipc_socket;

    // New connection per subscribe — the request/response IpcClient
    // in AppState is reserved for one-shot commands.
    let mut client = IpcClient::connect(&socket_path)
        .await
        .map_err(|e| format!("ipc connect: {e}"))?;
    client
        .subscribe(filter)
        .await
        .map_err(|e| format!("subscribe: {e}"))?;

    tokio::spawn(async move {
        loop {
            match client.next_event().await {
                Ok(ev) => {
                    // NOTE: must NOT log per-event here. The daemon re-emits log
                    // lines as Event::LogRecord onto the same bus this relay
                    // streams — a per-event log would become a new event and
                    // create an infinite amplification loop.
                    if channel.send(ev).is_err() {
                        // Receiver gone — Svelte unmounted the consumer. Normal.
                        break;
                    }
                }
                Err(e) => {
                    // Stream died (daemon gone / socket closed). Signal the
                    // frontend so it can re-subscribe instead of freezing.
                    let _ = app.emit("ipc:stream-closed", format!("{e}"));
                    break;
                }
            }
        }
    });

    Ok(())
}
