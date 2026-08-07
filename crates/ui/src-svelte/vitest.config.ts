// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";
import { svelteTesting } from "@testing-library/svelte/vite";

export default defineConfig({
  // svelteTesting() adds `browser` to resolve.conditions (for Svelte 5 client
  // module selection) and injects the @testing-library/svelte auto-cleanup
  // setup file. autoCleanup:false prevents the setup file from being added
  // globally — the tokens.css.test.ts runs under @vitest-environment node and
  // importing Svelte's client module there throws (no requestAnimationFrame).
  plugins: [sveltekit(), svelteTesting({ autoCleanup: false })],
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
