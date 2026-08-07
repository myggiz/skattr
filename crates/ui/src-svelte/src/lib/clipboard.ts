// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

// Copy text to the system clipboard via the Tauri clipboard-manager plugin.
//
// The web `navigator.clipboard.writeText` API does not work in the webkit2gtk
// webview on Wayland (restricted / no user-gesture bridge), so all copy actions
// must go through the plugin, which uses the Rust-side system clipboard.
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

export async function copyText(text: string): Promise<void> {
  await writeText(text);
}
