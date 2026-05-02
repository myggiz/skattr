// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Disable SSR globally — Skattr UI is a pure SPA running inside Tauri.
// This prevents vite-plugin-svelte's SSR transform path from hitting the
// esrap@1.4.9 TypeScript annotation bug during development.
export const ssr = false;
