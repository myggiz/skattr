// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";

export type ConnState = "live" | "reconnecting" | "dead";
export const connection = writable<{ state: ConnState }>({ state: "live" });

/** Test seam: reset to live. */
export function __resetForTest(): void {
  connection.set({ state: "live" });
}

interface RetryOpts { maxAttempts?: number; baseDelayMs?: number; }

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** On stream death: flip to reconnecting and retry `resubscribe` with bounded
 *  exponential backoff; → live on success, → dead after maxAttempts. */
export async function handleStreamClosed(
  resubscribe: () => Promise<void>,
  opts: RetryOpts = {},
): Promise<void> {
  const maxAttempts = opts.maxAttempts ?? 6;
  const baseDelayMs = opts.baseDelayMs ?? 500;
  connection.set({ state: "reconnecting" });
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    try {
      await resubscribe();
      connection.set({ state: "live" });
      return;
    } catch {
      const delay = Math.min(baseDelayMs * 2 ** attempt, 8000);
      if (delay > 0) await sleep(delay);
    }
  }
  connection.set({ state: "dead" });
}
