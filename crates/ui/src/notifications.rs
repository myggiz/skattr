// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Desktop notifications via `notify-rust`. Two Tauri commands:
//!   - `notify` — fires a single notification.
//!   - `focus_window_and_open_conversation` — invoked from the
//!     notification's click action to bring the window to the front
//!     and ask the SvelteKit shell to navigate.

use tauri::Manager;

#[tauri::command]
pub fn notify(title: String, body: String, conversation_id: Option<String>) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(&title).body(&body).appname("Skattr");
    #[cfg(target_os = "linux")]
    {
        if let Some(id) = conversation_id.as_ref() {
            n.hint(notify_rust::Hint::Custom(
                "conversation_id".into(),
                id.clone(),
            ));
        }
    }
    // On non-Linux platforms conversation_id is unused; suppress the lint.
    #[cfg(not(target_os = "linux"))]
    let _ = &conversation_id;
    n.show().map(|_| ()).map_err(|e| format!("notify: {e}"))
}

#[tauri::command]
pub fn focus_window_and_open_conversation(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    // Ask the SvelteKit shell to navigate via a custom event.
    // Sanitise the id for JSON injection: only allow hex digits and colons.
    if !id.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
        return Err("invalid conversation id".into());
    }
    w.eval(format!(
        r#"window.dispatchEvent(new CustomEvent('skattr:open-conversation', {{ detail: {{ id: "{id}" }} }}));"#
    ))
    .map_err(|e| e.to_string())?;
    Ok(())
}
