// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Snapshot regression guard for design tokens.
// Prevents accidental drift of colour, spacing, or typography tokens.
//
// The locked values below are the "Circuit" palette, which superseded the
// Phase 2.C one. Changing a token here is a design decision, so it has to be
// made deliberately in two places rather than slipping in with a component.
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
    "--live",
    "--hairline",
    "--bg-sunk",
    "--danger",
    "--danger-on-accent",
    "--s-1",
    "--s-2",
    "--s-3",
    "--s-4",
    "--t-body",
    "--t-ui",
    "--t-display",
    "--t-mono",
    "--t-label",
  ];
  for (const prop of required) {
    expect(css, `missing token ${prop}`).toContain(prop);
  }
});

test("tokens.css dark-mode palette values are locked", () => {
  expect(css).toContain("--bg: #0f1417");
  expect(css).toContain("--bg-elevated: #161d21");
  expect(css).toContain("--hairline: #232d32");
  expect(css).toContain("--text: #e4e9ea");
  expect(css).toContain("--text-muted: #8a989c");
  expect(css).toContain("--accent: #d09a45");
  expect(css).toContain("--live: #3ad8e8");
  expect(css).toContain("--danger: #c96a50");
});

test("tokens.css light-mode palette values are locked", () => {
  expect(css).toContain("--bg: #edf0ef");
  expect(css).toContain("--accent: #8a6420");
  expect(css).toContain("--live: #0b6672");
  expect(css).toContain("--danger: #a4402a");
});

test("the two light palettes are identical", () => {
  // The light values are written twice on purpose: once under
  // prefers-color-scheme (which must not stamp data-theme, so "system" keeps
  // following the OS) and once under [data-theme="light"] (which must beat a
  // dark OS). Nothing but this test stops the two copies from drifting, and a
  // drift would show as an explicit light choice looking subtly wrong.
  const decls = (block: string) =>
    [...block.matchAll(/(--[a-z-]+):\s*([^;]+);/g)].map((m) => `${m[1]}: ${m[2].trim()}`);

  const media = css.slice(css.indexOf("prefers-color-scheme: light"));
  const mediaBlock = media.slice(media.indexOf("{", media.indexOf(":root")), media.indexOf("\n  }"));
  const attrStart = css.indexOf('[data-theme="light"]');
  const attrBlock = css.slice(attrStart, css.indexOf("\n}", attrStart));

  const inMedia = decls(mediaBlock);
  const inAttr = decls(attrBlock);
  expect(inAttr.length).toBeGreaterThan(0);
  expect(inMedia).toEqual(inAttr);
});

test("both self-hosted display and data faces ship with the app", () => {
  // The CSP is 'self' and the app runs offline, so a font that is only named
  // in a stack and never bundled fails silently to a system fallback.
  expect(css).toContain('url("./fonts/familjen-grotesk-variable.woff2")');
  expect(css).toContain('url("./fonts/martian-mono-variable.woff2")');
});
