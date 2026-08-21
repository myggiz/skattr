// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// #197: --accent is a FILL (sent bubbles, buttons), not just an accent line, so
// anything drawn on top of it needs contrast against it. The light theme never
// overrode a dark-tuned accent, which left sent-message text at 2.41:1 and the
// delivery icon at 1.05:1 — a message you had sent was hard to read, and its
// delivery state unreadable.
//
// The snapshot test next door catches *that a token changed*; this catches
// *that a change is unreadable*, which is the property that actually matters.
//
// @vitest-environment node

import { expect, test, vi } from "vitest";

const fsModule = (await vi.importActual("fs")) as {
  readFileSync: (path: string, encoding: string) => string;
};
const css: string = fsModule.readFileSync(
  new URL("tokens.css", import.meta.url).pathname,
  "utf-8",
);

/** WCAG 2.2 relative luminance. */
function luminance(hex: string): number {
  const h = hex.replace("#", "");
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
  const f = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function contrast(a: string, b: string): number {
  const [la, lb] = [luminance(a), luminance(b)];
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * Resolve a token for one theme. The light theme is a `prefers-color-scheme`
 * block that overrides only some tokens, so an unlisted token falls back to
 * its `:root` value — the exact mechanism that let a dark-tuned accent leak
 * into light mode unnoticed.
 */
function token(name: string, theme: "dark" | "light"): string {
  const lightBlock = css.slice(css.indexOf("prefers-color-scheme: light"));
  const find = (haystack: string) =>
    haystack.match(new RegExp(`${name}:\\s*(#[0-9a-fA-F]{6})`))?.[1];
  if (theme === "light") {
    const override = find(lightBlock);
    if (override) return override;
  }
  const root = find(css.slice(0, css.indexOf("prefers-color-scheme: light")));
  if (!root) throw new Error(`token ${name} not found`);
  return root;
}

for (const theme of ["dark", "light"] as const) {
  test(`${theme}: text on an --accent fill is readable (WCAG 4.5:1)`, () => {
    // Accent-filled surfaces set `color: var(--bg)` (outgoing message bubble,
    // outgoing file bubble, rail button hover).
    const ratio = contrast(token("--accent", theme), token("--bg", theme));
    expect(ratio, `--bg on --accent in ${theme} is ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
  });

  test(`${theme}: the failed-delivery red is visible on an --accent fill (3:1)`, () => {
    // The plain --danger is ~1.3:1 on the accent, i.e. invisible, which is why
    // a separate on-accent value exists.
    const ratio = contrast(token("--accent", theme), token("--danger-on-accent", theme));
    expect(ratio, `--danger-on-accent on --accent in ${theme} is ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
  });

  test(`${theme}: --accent is readable as text on the page background (3:1)`, () => {
    const ratio = contrast(token("--accent", theme), token("--bg", theme));
    expect(ratio).toBeGreaterThanOrEqual(3);
  });
}
