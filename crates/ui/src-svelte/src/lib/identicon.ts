// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// A contact's avatar is derived from their public key, so it is a visual
// fingerprint: it cannot be chosen, and an impersonator with a different key
// cannot reproduce it. Initials or a picked image would carry no such promise.
//
// This is NOT a cryptographic check — the figure has far fewer bits than the
// key. It is a first-glance signal; the fingerprint in the contact panel
// remains the thing to compare.

/** Cells per side. Odd, so the mirrored figure has a spine. */
export const GRID = 5;

/** FNV-1a over the key's characters. Deterministic and dependency-free. */
function fold(key: string): number[] {
  const out: number[] = [];
  let h = 0x811c9dc5;
  // One pass per byte we need, re-mixing so consecutive bytes differ.
  for (let i = 0; i < GRID * GRID; i++) {
    for (const ch of `${key}#${i}`) {
      h ^= ch.charCodeAt(0);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    out.push(h & 0xff);
  }
  return out;
}

/**
 * The lit cells of the figure, row-major, mirrored across the vertical axis.
 * Always at least one cell lit and at least one clear.
 */
export function identiconCells(key: string): boolean[] {
  const bytes = fold(key);
  const half = Math.ceil(GRID / 2);
  const cells = new Array<boolean>(GRID * GRID).fill(false);

  for (let row = 0; row < GRID; row++) {
    for (let col = 0; col < half; col++) {
      // Bias slightly toward "lit" in the middle columns so the figure has a
      // body rather than scattered dots.
      const threshold = col === half - 1 ? 96 : 128;
      const on = bytes[row * half + col] > threshold;
      cells[row * GRID + col] = on;
      cells[row * GRID + (GRID - 1 - col)] = on;
    }
  }

  // Degenerate figures are unusable as avatars; force the spine on or off.
  const lit = cells.filter(Boolean).length;
  const spine = Math.floor(GRID / 2);
  if (lit === 0) cells[spine * GRID + spine] = true;
  if (lit === GRID * GRID) cells[spine] = false;
  return cells;
}

/** Hue in degrees for this key. Saturation and lightness are set by the theme. */
export function identiconHue(key: string): number {
  const bytes = fold(key);
  return ((bytes[0] << 8) | bytes[1]) % 360;
}
