// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Skattr UI — Tauri 2 + SvelteKit shell.
//!
//! Boots the Tauri runtime with two-phase Tauri command surfaces:
//! pre-daemon (`bootstrap`) and post-daemon (`ipc_bridge` + `events`).

mod bootstrap;
mod daemon;
mod events;
mod ipc_bridge;

use tauri::Manager;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,skattr=debug")),
        )
        .init();

    tauri::Builder::default()
        .manage(daemon::AppState::default())
        .invoke_handler(tauri::generate_handler![
            bootstrap::vault_exists,
            bootstrap::identity_init,
            bootstrap::vault_unlock,
            ipc_bridge::ipc_request,
            events::ipc_subscribe,
            daemon::start_in_process_cmd,
        ])
        .setup(|app| {
            // Resolve data_dir once and stash it.
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("Tauri app data dir")
                .join("skattr");
            std::fs::create_dir_all(&data_dir).ok();
            let state: tauri::State<daemon::AppState> = app.state();
            *state.data_dir.write() = Some(data_dir);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Phase 2.C: quit-on-close. 2.F replaces with hide-to-tray.
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    daemon::shutdown(&app).await;
                    app.exit(0);
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("Tauri run");
}
