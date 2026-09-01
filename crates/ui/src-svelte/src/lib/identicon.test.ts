// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// An identicon is not decoration here: it is a visual rendering of the
// contact's public key, so two different keys must not look the same and one
// key must never change appearance between sessions.
//
// @vitest-environment node

import { expect, test } from "vitest";
import { identiconCells, identiconHue, GRID } from "./identicon";

const KEY_A = "9f2ca41d77b80e635d19cc028fa431b74e6c1a0daa39b52f0c7d81e3b6a4f902";
const KEY_B = "9f2ca41d77b80e635d19cc028fa431b74e6c1a0daa39b52f0c7d81e3b6a4f903";

test("the same key always renders the same figure", () => {
  expect(identiconCells(KEY_A)).toEqual(identiconCells(KEY_A));
  expect(identiconHue(KEY_A)).toBe(identiconHue(KEY_A));
});

test("a one-character difference changes the figure", () => {
  // Keys differing only in their last nibble are exactly the pair a user is
  // most likely to be shown side by side by an impersonation attempt.
  expect(identiconCells(KEY_B)).not.toEqual(identiconCells(KEY_A));
});

test("the figure is mirrored, so it reads as a face rather than noise", () => {
  const cells = identiconCells(KEY_A);
  for (let row = 0; row < GRID; row++) {
    for (let col = 0; col < GRID; col++) {
      const mirrored = GRID - 1 - col;
      expect(cells[row * GRID + col]).toBe(cells[row * GRID + mirrored]);
    }
  }
});

test("the grid is fully populated", () => {
  expect(identiconCells(KEY_A)).toHaveLength(GRID * GRID);
});

test("a figure is never blank and never solid", () => {
  // Either extreme is unusable as an avatar. Sample a spread of keys rather
  // than one, since this is a property of the derivation, not of one input.
  for (let i = 0; i < 64; i++) {
    const key = (i * 2654435761).toString(16).padStart(64, "0");
    const on = identiconCells(key).filter(Boolean).length;
    expect(on, `key ${key} lit ${on} cells`).toBeGreaterThan(0);
    expect(on).toBeLessThan(GRID * GRID);
  }
});

test("hue is a usable angle", () => {
  for (const key of [KEY_A, KEY_B, "", "zz"]) {
    const h = identiconHue(key);
    expect(h).toBeGreaterThanOrEqual(0);
    expect(h).toBeLessThan(360);
    expect(Number.isFinite(h)).toBe(true);
  }
});
