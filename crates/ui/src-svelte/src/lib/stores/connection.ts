// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";

export type ConnState = "live" | "reconnecting" | "dead";
export const connection = writable<{ state: ConnState }>({ state: "live" });

/** Module-level reentrancy guard: prevents concurrent handleStreamClosed runs
 *  from racing and leaving two live subscriptions. */
let reconnecting = false;

/** Test seam: reset to live. */
export function __resetForTest(): void {
  connection.set({ state: "live" });
  reconnecting = false;
}

interface RetryOpts { maxAttempts?: number; baseDelayMs?: number; }

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** On stream death: flip to reconnecting and retry `resubscribe` with bounded
 *  exponential backoff; → live on success, → dead after maxAttempts.
 *  Concurrent calls are dropped (the first in-flight run wins). */
export async function handleStreamClosed(
  resubscribe: () => Promise<void>,
  opts: RetryOpts = {},
): Promise<void> {
  if (reconnecting) return;
  reconnecting = true;
  const maxAttempts = opts.maxAttempts ?? 6;
  const baseDelayMs = opts.baseDelayMs ?? 500;
  connection.set({ state: "reconnecting" });
  try {
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      try {
        await resubscribe();
        connection.set({ state: "live" });
        return;
      } catch {
        // Skip the sleep on the final attempt — no further retry will follow,
        // so delaying only postpones the "dead" transition the user sees.
        if (attempt < maxAttempts - 1) {
          const delay = Math.min(baseDelayMs * 2 ** attempt, 8000);
          if (delay > 0) await sleep(delay);
        }
      }
    }
    connection.set({ state: "dead" });
  } finally {
    reconnecting = false;
  }
}
