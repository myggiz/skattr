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
const urlModule = (await vi.importActual("url")) as {
  fileURLToPath: (u: URL | string) => string;
};
// fileURLToPath, not `.pathname`: the latter stays percent-encoded and would
// break on a checkout path containing a space or non-ASCII character.
const css: string = fsModule.readFileSync(
  urlModule.fileURLToPath(new URL("tokens.css", import.meta.url)),
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

/**
 * Composite a foreground over a backdrop at `alpha`, the way CSS `opacity`
 * does. Token-level contrast is not the whole story: a rule that dims an
 * inherited colour toward an accent fill can fail even when both tokens are
 * fine, which is exactly what a review caught on the timestamp here.
 */
function composite(fg: string, bg: string, alpha: number): string {
  const parse = (h: string) => [0, 2, 4].map((i) => parseInt(h.replace("#", "").slice(i, i + 2), 16));
  const [f, b] = [parse(fg), parse(bg)];
  const mix = f.map((c, i) => Math.round(c * alpha + b[i] * (1 - alpha)));
  return `#${mix.map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

/** Read a component's stylesheet so the assertions track the real rules. */
function component(name: string): string {
  return fsModule.readFileSync(
    urlModule.fileURLToPath(new URL(`components/${name}`, import.meta.url)),
    "utf-8",
  );
}

for (const theme of ["dark", "light"] as const) {
  test(`${theme}: the outgoing-bubble timestamp meets text contrast (4.5:1)`, () => {
    // `.bubble.outgoing` sets `color: var(--bg)`, and `.ts` inherits it.
    // A timestamp is text, so WCAG 1.4.3 applies — 3:1 is not enough.
    const css = component("MessageBubble.svelte");
    const rule = css.match(/\.bubble\.outgoing \.ts \{[^}]*\}/)?.[0] ?? "";
    const alpha = Number(rule.match(/opacity:\s*([0-9.]+)/)?.[1] ?? 1);
    const accent = token("--accent", theme);
    const ratio = contrast(accent, composite(token("--bg", theme), accent, alpha));
    expect(ratio, `timestamp at opacity ${alpha} in ${theme} is ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
  });

  test(`${theme}: delivery icons meet non-text contrast (3:1)`, () => {
    // Icons are non-text UI components (WCAG 1.4.11), so 3:1 applies. They may
    // legitimately be dimmed relative to the delivered state, but not below it.
    const css = component("DeliveryIcon.svelte");
    const accent = token("--accent", theme);
    for (const state of ["pending", "sent", "delivered"]) {
      const rule = css.match(new RegExp(`\\.${state}\\s+:global\\(svg\\) \\{[^}]*\\}`))?.[0] ?? "";
      const alpha = Number(rule.match(/opacity:\s*([0-9.]+)/)?.[1] ?? 1);
      const ratio = contrast(accent, composite(token("--bg", theme), accent, alpha));
      expect(ratio, `${state} icon at opacity ${alpha} in ${theme} is ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
    }
  });

  test(`${theme}: the search palette's own text is readable`, () => {
    // Regression guard for a self-inflicted one: an unanchored sed during a
    // mutation check rewrote every `color: var(--bg)` in this file, not just
    // the one on the accent-filled <mark>, which set the panel and the search
    // input to draw their text in the page-background colour — invisible.
    const css = component("SearchPalette.svelte");
    const pairs: Array<[string, string, string]> = [
      [".panel", "--bg-elevated", css.match(/\.panel \{[^}]*\}/)?.[0] ?? ""],
      ["input", "--bg", css.match(/input\[type="text"\] \{[^}]*\}/)?.[0] ?? ""],
    ];
    for (const [label, bgToken, rule] of pairs) {
      expect(rule, `${label} rule not found`).not.toBe("");
      const fgToken = rule.match(/color:\s*var\((--[a-z-]+)/)?.[1] ?? "--text";
      const ratio = contrast(token(bgToken, theme), token(fgToken, theme));
      expect(ratio, `${label} draws ${fgToken} on ${bgToken}: ${ratio.toFixed(2)}:1 in ${theme}`).toBeGreaterThanOrEqual(4.5);
    }
  });

  test(`${theme}: search-result highlight meets text contrast (4.5:1)`, () => {
    // The one accent fill that does not inherit --bg: it sets its own colour.
    const css = component("SearchPalette.svelte");
    const rule = css.match(/\.snippet :global\(mark\) \{[^}]*\}/)?.[0] ?? "";
    expect(rule, "mark rule not found").not.toBe("");
    const fgToken = rule.match(/color:\s*var\((--[a-z-]+)/)?.[1] ?? "--text";
    const ratio = contrast(token("--accent", theme), token(fgToken, theme));
    expect(ratio, `mark uses ${fgToken}: ${ratio.toFixed(2)}:1 in ${theme}`).toBeGreaterThanOrEqual(4.5);
  });
}
