// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

// Derive the absolute path to the tauri-mock without `node:path`/`__dirname`
// so this config is type-checked against the DOM+esnext tsconfig correctly.
const tauriMockPath = new URL(
  "src/lib/test/tauri-mock.ts",
  import.meta.url,
).pathname;

export default defineConfig({
  plugins: [sveltekit()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_"],
  resolve:
    process.env["TAURI_MOCK"] === "1"
      ? {
          alias: {
            "@tauri-apps/api/core": tauriMockPath,
          },
        }
      : undefined,
});
