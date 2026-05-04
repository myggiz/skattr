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
mod notifications;
pub mod tray;

use tauri::Manager;

fn main() {
    use skattr_core::daemon::logs::{LogSink, RingBufferLayer};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // Create the log sink before building the subscriber so both the
    // tracing layer and the daemon share the same ring buffer allocation.
    let log_sink = LogSink::new();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,skattr=debug")),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(RingBufferLayer::new(log_sink.clone()))
        .init();

    let app_state = daemon::AppState {
        log_sink,
        ..daemon::AppState::default()
    };

    let result = tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            bootstrap::vault_exists,
            bootstrap::identity_init,
            bootstrap::vault_unlock,
            ipc_bridge::ipc_request,
            ipc_bridge::render_invite_qr,
            events::ipc_subscribe,
            daemon::start_in_process_cmd,
            notifications::notify,
            notifications::focus_window_and_open_conversation,
        ])
        .setup(|app| {
            // Resolve data_dir once and stash it. `app_data_dir` only fails
            // on platforms where Tauri can't determine a config root —
            // surface that as a Tauri setup error so the caller sees it.
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app_data_dir: {e}"))?
                .join("skattr");
            std::fs::create_dir_all(&data_dir).ok();
            let state: tauri::State<daemon::AppState> = app.state();
            *state.data_dir.write() = Some(data_dir);
            crate::tray::install(app.handle())?;
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
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "Tauri runtime exited with error");
        std::process::exit(1);
    }
}
