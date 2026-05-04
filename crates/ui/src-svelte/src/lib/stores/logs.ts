// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// logs store — backfills from TailLogs then subscribes to EventFilter::Logs.

import { writable } from "svelte/store";
import type { LogRecord, Event } from "$lib/ipc/types";
import { ipcClient } from "$lib/ipc/tauri";
import { tailLogs } from "$lib/stores/config";

const MAX_RECORDS = 500;

const _records = writable<LogRecord[]>([]);

/** Read-only view of the in-memory log ring. */
export const logs = { subscribe: _records.subscribe };

let _unsub: (() => void) | null = null;

/**
 * Backfill from the daemon ring buffer and start streaming live log
 * records. Safe to call multiple times — returns early if already
 * attached.
 */
export async function attachLogs(): Promise<void> {
  if (_unsub) return;

  // Backfill.
  try {
    const { records } = await tailLogs(null, MAX_RECORDS);
    _records.set(records);
  } catch {
    _records.set([]);
  }

  // Live tail.
  try {
    _unsub = await ipcClient.subscribe({ filter: "logs" }, (evt: Event) => {
      if (evt.event !== "log_record") return;
      const rec: LogRecord = evt.data;
      _records.update((rs) => {
        const next = [...rs, rec];
        return next.length > MAX_RECORDS ? next.slice(-MAX_RECORDS) : next;
      });
    });
  } catch {
    // Subscribe unavailable (e.g. test environment); rely on backfill only.
  }
}

/**
 * Stop streaming and clear the in-memory buffer. Call in onDestroy.
 */
export function detachLogs(): void {
  _unsub?.();
  _unsub = null;
  _records.set([]);
}
