// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Snapshot regression guard for design tokens.
// Prevents accidental drift of colour, spacing, or typography tokens.
//
// @vitest-environment node

import { expect, test, vi } from "vitest";

// Load the CSS as a raw string. We use `vi.importActual` so that TypeScript
// type-checks against `unknown` (no @types/node required), and we cast the
// result to a module with a `readFileSync` export.
const fsModule = (await vi.importActual("fs")) as {
  readFileSync: (path: string, encoding: string) => string;
};
const cssPath = new URL("tokens.css", import.meta.url).pathname;
const css: string = fsModule.readFileSync(cssPath, "utf-8");

test("tokens.css snapshot — no accidental token drift", () => {
  expect(css).toMatchSnapshot();
});

test("tokens.css contains all required custom properties", () => {
  const required = [
    "--bg",
    "--bg-elevated",
    "--text",
    "--text-muted",
    "--accent",
    "--danger",
    "--danger-on-accent",
    "--s-1",
    "--s-2",
    "--s-3",
    "--s-4",
    "--t-body",
    "--t-ui",
    "--t-display",
  ];
  for (const prop of required) {
    expect(css, `missing token ${prop}`).toContain(prop);
  }
});

test("tokens.css dark-mode palette values are locked", () => {
  // These are the Phase 2.C locked values from the spec.
  expect(css).toContain("--bg: #0e0f12");
  expect(css).toContain("--bg-elevated: #16181d");
  expect(css).toContain("--text: #e8eaed");
  expect(css).toContain("--text-muted: #9aa0a6");
  expect(css).toContain("--accent: #7aa2f7");
  expect(css).toContain("--danger: #ef4444");
});

test("tokens.css light-mode palette values are locked", () => {
  // Light mode overrides; T10 added --danger: #dc2626 in light.
  expect(css).toContain("--danger: #dc2626");
});
