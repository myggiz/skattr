// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Tauri 2 built-in system-tray. Menu items: Show window / Tor status
//! (disabled, status-only) / Unread count (disabled, status-only,
//! hidden when 0) / Quit. Click on the tray icon toggles window
//! visibility.
//!
//! No-tray environments (bare Wayland, some Linux desktops without a
//! StatusNotifier protocol implementation) are tolerated: tray init
//! failure is logged at WARN and the app keeps running. The
//! close-button handler in main.rs falls back to "quit" in that case
//! by reading a flag we set here.

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// Marker stored on the AppHandle's managed state so other code (e.g.
/// the close-button handler) can tell whether tray init succeeded.
#[derive(Debug, Default)]
pub struct TrayPresent(pub std::sync::atomic::AtomicBool);

/// Initialise the tray. Returns Ok(()) on success; on failure logs a
/// warning and returns Ok(()) anyway so the app keeps running.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    // Build the menu items.
    let header = MenuItemBuilder::new("Skattr")
        .id("header")
        .enabled(false)
        .build(app)?;
    let show = MenuItemBuilder::new("Show window").id("show").build(app)?;
    let tor_status = MenuItemBuilder::new("Tor: connecting…")
        .id("tor_status")
        .enabled(false)
        .build(app)?;
    let unread = MenuItemBuilder::new("Unread: 0")
        .id("unread")
        .enabled(false)
        .build(app)?;
    let quit = MenuItemBuilder::new("Quit").id("quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&header)
        .separator()
        .item(&show)
        .separator()
        .item(&tor_status)
        .item(&unread)
        .separator()
        .item(&quit)
        .build()?;

    // Build the tray icon. The default window icon is reused.
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    let result = TrayIconBuilder::new()
        .menu(&menu)
        .icon(icon)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Only toggle on a left-click *release*. Matching any `Click`
            // (issue #30) also fired on right-click — which opens the context
            // menu — and the window `set_focus()` here dismissed the
            // just-opened menu, making Quit unreachable. Filtering to
            // Left+Up also drops an accidental double-toggle on press+release.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(w) = tray.app_handle().get_webview_window("main") {
                    if w.is_visible().unwrap_or(false) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app);

    match result {
        Ok(_tray_icon) => {
            // Stash the presence flag so other handlers can check it.
            // `_tray_icon` is intentionally kept alive via managed state
            // (Tauri drops the icon when the last ref is dropped).
            app.manage(TrayPresent(std::sync::atomic::AtomicBool::new(true)));
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "tray init failed; continuing without tray");
            // Still install a TrayPresent marker, but with `false` so the
            // close-button handler can fall back to "quit on close".
            app.manage(TrayPresent(std::sync::atomic::AtomicBool::new(false)));
            Ok(())
        }
    }
}
