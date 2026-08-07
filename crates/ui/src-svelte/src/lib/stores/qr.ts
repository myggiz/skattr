// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { invoke } from "@tauri-apps/api/core";

const cache = new Map<string, string>();

export async function renderInviteQr(url: string): Promise<string> {
  const cached = cache.get(url);
  if (cached !== undefined) return cached;
  const svg = await invoke<string>("render_invite_qr", { url });
  cache.set(url, svg);
  return svg;
}

/** Test-only: clears the cache. */
export function _resetCacheForTest(): void {
  cache.clear();
}
