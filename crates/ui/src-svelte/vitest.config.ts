// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [sveltekit()],
  // Allow `?raw` imports of CSS files in tests.
  assetsInclude: [],
  test: {
    include: ["src/**/*.{test,spec}.{js,ts}"],
    environment: "jsdom",
    globals: true,
    // Ensure raw string imports work: treat CSS files as assets in test context.
    environmentOptions: {},
  },
});
