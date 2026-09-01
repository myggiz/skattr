// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// Inlines bundled Lucide icons (ISC) as raw SVG strings so they
// can be dropped into Svelte components via {@html ...}. Vite's
// `?raw` query parameter loads each file as text at build time.
// Glyphs are bundled with the LICENSE adjacent — no remote
// fetching, no CDN.

import clockSvg from "./clock.svg?raw";
import checkSvg from "./check.svg?raw";
import checkCheckSvg from "./check-check.svg?raw";
import alertTriangleSvg from "./alert-triangle.svg?raw";
import qrCodeSvg from "./qr-code.svg?raw";
import paperclipSvg from "./paperclip.svg?raw";
import fileSvg from "./file.svg?raw";
import imageSvg from "./image.svg?raw";
import searchSvg from "./search.svg?raw";
import settingsSvg from "./settings.svg?raw";
import sunSvg from "./sun.svg?raw";
import moonSvg from "./moon.svg?raw";
import monitorSvg from "./monitor.svg?raw";

export const icons = {
  clock: clockSvg,
  check: checkSvg,
  "check-check": checkCheckSvg,
  "alert-triangle": alertTriangleSvg,
  "qr-code": qrCodeSvg,
  paperclip: paperclipSvg,
  file: fileSvg,
  image: imageSvg,
  search: searchSvg,
  settings: settingsSvg,
  sun: sunSvg,
  moon: moonSvg,
  monitor: monitorSvg,
} as const;

export type IconName = keyof typeof icons;
