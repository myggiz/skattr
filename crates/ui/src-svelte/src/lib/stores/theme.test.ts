// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// The theme is one of three states, not a boolean: "system" is a real choice
// that has to survive a reload, and it is NOT the same as whichever of
// dark/light the OS currently reports. Stamping `data-theme` for it would
// freeze the page against a later OS change.
//
// @vitest-environment jsdom

import { describe, expect, test } from "vitest";
import { applyTheme, parseTheme, THEME_KEY } from "./theme";

describe("parseTheme", () => {
  test("accepts the three known values", () => {
    expect(parseTheme("dark")).toBe("dark");
    expect(parseTheme("light")).toBe("light");
    expect(parseTheme("system")).toBe("system");
  });

  test("falls back to system for anything else", () => {
    // Storage is untrusted input: a stale key, a hand-edited value, or null on
    // a first run must not leave the app unthemed.
    for (const junk of [null, "", "Dark", "solarized", "true"]) {
      expect(parseTheme(junk)).toBe("system");
    }
  });
});

describe("applyTheme", () => {
  test("an explicit choice stamps data-theme", () => {
    const root = document.createElement("html");
    applyTheme(root, "light");
    expect(root.getAttribute("data-theme")).toBe("light");
    applyTheme(root, "dark");
    expect(root.getAttribute("data-theme")).toBe("dark");
  });

  test("system REMOVES the stamp rather than writing one", () => {
    // The regression this guards: writing data-theme="dark" for "system"
    // looks identical on screen right now and is wrong the moment the OS
    // flips, because the media query can no longer win.
    const root = document.createElement("html");
    root.setAttribute("data-theme", "light");
    applyTheme(root, "system");
    expect(root.hasAttribute("data-theme")).toBe(false);
  });
});

test("the storage key is stable", () => {
  // Renaming it silently resets every existing user's choice.
  expect(THEME_KEY).toBe("skattr.theme");
});
