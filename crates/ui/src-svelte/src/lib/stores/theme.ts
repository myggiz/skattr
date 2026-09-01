// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// Theme selection: dark, light, or follow the OS.
//
// "system" is represented by the ABSENCE of a `data-theme` stamp, so the
// `prefers-color-scheme` rules in tokens.css stay in charge and a later OS
// change is picked up without the app doing anything.

import { writable } from "svelte/store";

export type Theme = "dark" | "light" | "system";

export const THEME_KEY = "skattr.theme";

/** Parse an untrusted stored value. Anything unrecognised means "system". */
export function parseTheme(raw: string | null): Theme {
  return raw === "dark" || raw === "light" ? raw : "system";
}

/** Stamp (or clear) the theme on the document root. */
export function applyTheme(root: Element, theme: Theme): void {
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
}

function readStored(): Theme {
  try {
    return parseTheme(localStorage.getItem(THEME_KEY));
  } catch {
    // Private mode or a locked-down webview can throw on access, not just
    // return null. A theme is a preference, so failing to read one is not an
    // error worth surfacing.
    return "system";
  }
}

export const theme = writable<Theme>(readStored());

/** Choose a theme: stamps the document and persists the choice. */
export function setTheme(next: Theme): void {
  theme.set(next);
  applyTheme(document.documentElement, next);
  try {
    localStorage.setItem(THEME_KEY, next);
  } catch {
    // Preference is lost on reload; the current session still honours it.
  }
}

export const THEME_ORDER: readonly Theme[] = ["system", "light", "dark"];

/** Next theme in the cycle, for a single-button control. */
export function nextTheme(current: Theme): Theme {
  const i = THEME_ORDER.indexOf(current);
  return THEME_ORDER[(i + 1) % THEME_ORDER.length];
}
