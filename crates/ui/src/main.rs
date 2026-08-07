// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Skattr UI — Tauri 2 + SvelteKit shell.
//!
//! Boots the Tauri runtime with two-phase Tauri command surfaces:
//! pre-daemon (`bootstrap`) and post-daemon (`ipc_bridge` + `events`).

mod attachments;
mod bootstrap;
mod daemon;
mod events;
mod ipc_bridge;
mod notifications;
pub mod tray;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tauri::Manager;

/// Parsed `--smoke-test` arguments.
#[derive(Debug)]
struct SmokeArgs {
    data_dir: std::path::PathBuf,
    timeout_secs: u64,
}

/// Returns true if `--smoke-test` appears anywhere in argv.
fn detect_smoke_test_flag(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--smoke-test")
}

/// Parse `--smoke-test`-mode argv. Required: `--data-dir <PATH>`.
/// Optional: `--timeout-secs <N>` (default 240).
fn parse_smoke_argv(argv: &[String]) -> Result<SmokeArgs, String> {
    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut timeout_secs: u64 = 240;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--data-dir" => {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| "--data-dir requires a value".to_string())?;
                data_dir = Some(std::path::PathBuf::from(v));
                i += 2;
            }
            "--timeout-secs" => {
                let v = argv
                    .get(i + 1)
                    .ok_or_else(|| "--timeout-secs requires a value".to_string())?;
                timeout_secs = v
                    .parse::<u64>()
                    .map_err(|e| format!("--timeout-secs not a number: {e}"))?;
                i += 2;
            }
            _ => i += 1,
        }
    }
    Ok(SmokeArgs {
        data_dir: data_dir.ok_or_else(|| "--smoke-test requires --data-dir".to_string())?,
        timeout_secs,
    })
}

/// Argv branch invoked from `main()` when `--smoke-test` is present.
/// Builds a `SmokeConfig`, runs `core::daemon::smoke::run_smoke` on a
/// fresh Tokio runtime, prints the report (or error), and exits.
fn run_smoke_and_exit(argv: Vec<String>) -> ! {
    let parsed = match parse_smoke_argv(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("smoke: argv error: {e}");
            std::process::exit(2);
        }
    };
    let cfg = skattr_core::daemon::smoke::SmokeConfig {
        data_dir: parsed.data_dir,
        tor_ready_timeout: std::time::Duration::from_secs(parsed.timeout_secs),
        ..Default::default()
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("smoke: tokio runtime build failed: {e}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(skattr_core::daemon::smoke::run_smoke(cfg)) {
        Ok(report) => {
            println!(
                "smoke OK: onion={} duration={:?}",
                report.onion, report.duration
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("smoke FAIL: {e}");
            std::process::exit(1);
        }
    }
}

/// Synchronous mirror of `Config.ui.close_to_tray`, kept on Tauri managed
/// state so the synchronous `on_window_event` closure can read it without
/// entering an async context.
///
/// Initialised at startup from the on-disk config; updated at runtime by the
/// SvelteKit shell via [`set_close_to_tray`] after every successful
/// `SetConfig` round-trip.
#[derive(Debug)]
pub struct CloseToTraySentinel(pub Arc<AtomicBool>);

impl Default for CloseToTraySentinel {
    fn default() -> Self {
        // Default true; overwritten in setup from the on-disk config.
        Self(Arc::new(AtomicBool::new(true)))
    }
}

/// Tauri command: update the in-process `close_to_tray` sentinel.
///
/// The SvelteKit config store calls this after every successful `SetConfig`
/// round-trip so that the close-button handler stays in sync without reading
/// the async `RwLock<Config>` from a synchronous event closure.
#[tauri::command]
fn set_close_to_tray(state: tauri::State<'_, CloseToTraySentinel>, enabled: bool) {
    state.0.store(enabled, Ordering::SeqCst);
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if detect_smoke_test_flag(&argv) {
        run_smoke_and_exit(argv);
    }

    use skattr_core::daemon::logs::{LogSink, RingBufferLayer};
    use tracing_subscriber::filter::{EnvFilter, LevelFilter};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

    // Create the log sink before building the subscriber so both the
    // tracing layer and the daemon share the same ring buffer allocation.
    let log_sink = LogSink::new();

    // The canonical data dir (~/.local/share/skattr on Linux/macOS,
    // %LOCALAPPDATA%\skattr on Windows). Resolved via the shared path
    // resolver so it is stable regardless of the Tauri app identifier
    // (identifier "net.myggiz.skattr" would give a different XDG path
    // and strand existing data).
    let data_dir = match skattr_core::daemon::data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("fatal: cannot resolve data dir: {e}");
            std::process::exit(1);
        }
    };

    // Run the idempotent legacy migration BEFORE reading persist_logs from
    // config: on the first migrated launch a legacy ui.persist_logs_to_disk
    // value lives in the old location. Without this early call the setting
    // would be invisible and skattr.log would be created against the user's
    // preference. The setup hook below calls migrate_legacy_into a second time;
    // idempotency makes that a no-op.
    if let Err(e) = skattr_core::daemon::migrate_legacy_into(&data_dir) {
        eprintln!("fatal: data migration failed: {e}");
        std::process::exit(1);
    }

    // Optional on-disk log. Default ON for the 0.1.x field-testing line
    // (temporary — revert before 1.0, see #86): writes <data_dir>/skattr.log so
    // testers can produce diagnostics without hunting for a setting. Controlled
    // by ui.persist_logs_to_disk. The `unwrap_or` matches the shipped default
    // (`default_persist_logs`) for the rare case where config load *and*
    // defaults both fail.
    let persist_logs = {
        let p = data_dir.join("config.toml");
        skattr_core::daemon::Config::load(&p)
            .or_else(|_| skattr_core::daemon::Config::defaults())
            .map(|c| c.ui.persist_logs_to_disk)
            .unwrap_or(true)
    };
    let file_layer = if persist_logs {
        let log_dir = data_dir.clone();
        let _ = std::fs::create_dir_all(&log_dir);
        let appender = tracing_appender::rolling::never(&log_dir, "skattr.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        // Keep the writer alive for the whole process so buffered lines flush.
        Box::leak(Box::new(guard));
        // Disk file gets DEBUG (its own per-layer filter), independent of the
        // bus level below and of RUST_LOG, so the shared log is always useful.
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking)
                .with_filter(EnvFilter::new("info,skattr_core=debug,skattr_ui=debug"))
                .boxed(),
        )
    } else {
        None
    };

    // stderr honors RUST_LOG, defaulting to info.
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(stderr_filter),
        )
        // The ring buffer feeds the in-app LogsViewer AND is re-emitted as
        // Event::LogRecord onto the IPC event bus. Cap it at INFO: high-volume
        // DEBUG (which still reaches the disk file above) would otherwise flood
        // the broadcast bus and lag/drop real events (messages, contact updates).
        .with(RingBufferLayer::new(log_sink.clone()).with_filter(LevelFilter::INFO))
        .with(file_layer)
        .init();

    let app_state = daemon::AppState {
        log_sink,
        ..daemon::AppState::default()
    };

    let result = tauri::Builder::default()
        .manage(app_state)
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Forward subsequent skattr:// invocations into the
            // existing process. The deep-link plugin will fire its
            // own event for any URL in argv; we just need the
            // window to come back into focus.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
            tracing::debug!(?argv, "single-instance: forwarded launch");
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            bootstrap::vault_exists,
            bootstrap::identity_init,
            bootstrap::vault_unlock,
            bootstrap::reset_local_data,
            ipc_bridge::ipc_request,
            ipc_bridge::render_invite_qr,
            events::ipc_subscribe,
            daemon::start_in_process_cmd,
            daemon::daemon_running,
            notifications::notify,
            notifications::focus_window_and_open_conversation,
            set_close_to_tray,
            attachments::decode_attachment_manifest,
            attachments::file_size,
            attachments::open_file,
            attachments::reveal_in_folder,
        ])
        .setup(|app| {
            // The canonical data dir — shared resolver keeps this in sync with
            // the daemon and the CLI regardless of the Tauri app identifier.
            let data_dir = skattr_core::daemon::data_dir()
                .map_err(|e| format!("resolve data dir: {e}"))?;
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("create data dir: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = std::fs::set_permissions(
                    &data_dir,
                    std::fs::Permissions::from_mode(0o700),
                ) {
                    tracing::warn!(error = %e, "could not set data_dir permissions to 0700 (non-fatal)");
                }
            }

            // One-time migration from the previous split layout
            // (~/.local/share/net.myggiz.skattr/skattr/ for data, ~/.config/skattr
            // for config) into the consolidated dir, so existing identities carry
            // over rather than being silently abandoned.
            skattr_core::daemon::migrate_legacy_into(&data_dir)
                .map_err(|e| format!("data migration failed: {e}"))?;

            let state: tauri::State<daemon::AppState> = app.state();

            // Config now lives inside the data dir. Load it to resolve the
            // download dir + UI prefs. (Falls back to defaults on first run.)
            let config_path = data_dir.join("config.toml");
            let mut full_cfg = match skattr_core::daemon::Config::load(&config_path) {
                Ok(c) => c,
                Err(_) => skattr_core::daemon::Config::defaults()
                    .map_err(|e| format!("config defaults: {e}"))?,
            };
            full_cfg.data_dir = data_dir.clone();
            let ui_cfg = full_cfg.ui.clone();

            *state.data_dir.write() = Some(data_dir);

            // Initialise the close-to-tray sentinel from the persisted value.
            let sentinel = CloseToTraySentinel(Arc::new(AtomicBool::new(ui_cfg.close_to_tray)));
            app.manage(sentinel);

            crate::tray::install(app.handle())?;

            // Hide the window immediately if start_minimised is set.
            if ui_cfg.start_minimised {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            // Forward incoming skattr:// deep-link events into the
            // SvelteKit shell as a CustomEvent the Add-Contact dialog
            // listens for via the `deepLinkInviteUrl` store.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_for_links = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> =
                        event.urls().into_iter().map(|u| u.to_string()).collect();
                    if urls.is_empty() {
                        return;
                    }
                    if let Some(wv) = app_for_links.get_webview_window("main") {
                        let _ = wv.show();
                        let _ = wv.set_focus();
                        let payload = match serde_json::to_string(&urls[0]) {
                            Ok(s) => s,
                            Err(_) => "\"\"".to_string(),
                        };
                        let js = format!(
                            "window.dispatchEvent(new CustomEvent('skattr:deep-link', {{ detail: {payload} }}));"
                        );
                        let _ = wv.eval(&js);
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();

                // Only intercept close if:
                //   1. A tray icon was successfully created (graceful
                //      degradation for Wayland / no-StatusNotifier desktops).
                //   2. The user has close_to_tray enabled.
                let tray_present = app
                    .try_state::<crate::tray::TrayPresent>()
                    .map(|s| s.0.load(Ordering::SeqCst))
                    .unwrap_or(false);
                let close_to_tray = app
                    .try_state::<CloseToTraySentinel>()
                    .map(|s| s.0.load(Ordering::SeqCst))
                    .unwrap_or(true);

                if tray_present && close_to_tray {
                    // Prevent the default close; hide instead.
                    api.prevent_close();
                    let _ = window.hide();
                    // Notify the SvelteKit shell so it can show a one-time
                    // "minimised to tray" toast (suppressed on subsequent
                    // hides via localStorage — implemented in Task 36).
                    // `window` here is `&tauri::Window`; eval lives on
                    // `WebviewWindow`, so we reach it via get_webview_window.
                    if let Some(wv) = app.get_webview_window("main") {
                        let _ = wv.eval(
                            "window.dispatchEvent(new CustomEvent('skattr:close-to-tray-hidden'));",
                        );
                    }
                } else {
                    // No tray or close_to_tray disabled: normal quit path.
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn(async move {
                        daemon::shutdown(&app).await;
                        app.exit(0);
                    });
                }
            }
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "Tauri runtime exited with error");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod smoke_argv_tests {
    use super::*;

    #[test]
    fn smoke_argv_detects_flag() {
        let argv = vec![
            "skattr-ui".to_string(),
            "--smoke-test".to_string(),
            "--data-dir".to_string(),
            "/tmp/foo".to_string(),
        ];
        let parsed = parse_smoke_argv(&argv).expect("parses cleanly");
        assert_eq!(parsed.data_dir.to_str(), Some("/tmp/foo"));
        assert_eq!(parsed.timeout_secs, 240);
    }

    #[test]
    fn smoke_argv_accepts_timeout_override() {
        let argv = vec![
            "skattr-ui".to_string(),
            "--smoke-test".to_string(),
            "--data-dir".to_string(),
            "/tmp/foo".to_string(),
            "--timeout-secs".to_string(),
            "60".to_string(),
        ];
        let parsed = parse_smoke_argv(&argv).unwrap();
        assert_eq!(parsed.timeout_secs, 60);
    }

    #[test]
    fn smoke_argv_rejects_missing_data_dir() {
        let argv = vec!["skattr-ui".to_string(), "--smoke-test".to_string()];
        let err = parse_smoke_argv(&argv).unwrap_err();
        assert!(err.contains("--data-dir"));
    }

    #[test]
    fn smoke_argv_returns_none_when_flag_absent() {
        let argv = vec!["skattr-ui".to_string()];
        let result = detect_smoke_test_flag(&argv);
        assert!(!result, "no flag => false");
    }

    #[test]
    fn smoke_argv_returns_true_when_flag_present() {
        let argv = vec!["skattr-ui".to_string(), "--smoke-test".to_string()];
        let result = detect_smoke_test_flag(&argv);
        assert!(result, "flag present => true");
    }
}
