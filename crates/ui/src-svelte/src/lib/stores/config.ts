// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
//
// config store — fetches ConfigSnapshot from the daemon, exposes an
// optimistic patchConfig helper that coalesces edits over 500 ms then
// flushes via SetConfig. On flush failure, authoritative state is
// re-fetched from the daemon.

import { writable, get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import type { ConfigSnapshot, ConfigPatch, LogRecord } from "$lib/ipc/types";
import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import { errorMessage } from "$lib/ipc/errors";

// ---------------------------------------------------------------------------
// Config store
// ---------------------------------------------------------------------------

const CACHE_TTL_MS = 5_000;

interface ConfigState {
  snapshot: ConfigSnapshot | null;
  fetchedAt: number;
}

const _state = writable<ConfigState>({ snapshot: null, fetchedAt: 0 });

/** Read-only derived view; subscribe for reactive config access. */
export const config = { subscribe: _state.subscribe };

/** Fetch config from the daemon (cached for CACHE_TTL_MS). */
export async function fetchConfig(): Promise<ConfigSnapshot> {
  const cur = get(_state);
  if (cur.snapshot && Date.now() - cur.fetchedAt < CACHE_TTL_MS) {
    return cur.snapshot;
  }
  const resp = await ipcClient.request({ cmd: "get_config" });
  const result = unwrapOk(resp);
  if (result.result !== "config") {
    throw new Error(`unexpected reply: ${result.result}`);
  }
  const snap = result.data;
  _state.set({ snapshot: snap, fetchedAt: Date.now() });
  return snap;
}

let _saveTimer: ReturnType<typeof setTimeout> | null = null;
let _pendingPatch: ConfigPatch = {
  history_retention_days: null,
  direct_timeout_secs: null,
  notification_mode: null,
  close_to_tray: null,
  start_minimised: null,
  persist_logs_to_disk: null,
  download_dir: null,
};

/**
 * Optimistically update the visible snapshot and debounce the flush.
 * Coalesces multiple calls within 500 ms into a single SetConfig call.
 * On flush error, rolls back by re-fetching authoritative state.
 */
export function patchConfig(patch: ConfigPatch): Promise<void> {
  // Merge into pending batch.
  _pendingPatch = { ..._pendingPatch, ...patch };

  // Optimistic update of the visible snapshot.
  _state.update((s) => {
    if (!s.snapshot) return s;
    const merged: ConfigSnapshot = { ...s.snapshot };
    for (const [k, v] of Object.entries(patch) as [keyof ConfigPatch, unknown][]) {
      if (v !== undefined && v !== null) {
        (merged as Record<string, unknown>)[k] = v;
      }
    }
    return { snapshot: merged, fetchedAt: Date.now() };
  });

  return new Promise((resolve, reject) => {
    if (_saveTimer) clearTimeout(_saveTimer);
    _saveTimer = setTimeout(async () => {
      const toSend = _pendingPatch;
      // Reset pending immediately so a concurrent patchConfig starts fresh.
      _pendingPatch = {
        history_retention_days: null,
        direct_timeout_secs: null,
        notification_mode: null,
        close_to_tray: null,
        start_minimised: null,
        persist_logs_to_disk: null,
        download_dir: null,
      };
      try {
        const resp = await ipcClient.request({ cmd: "set_config", patch: toSend });
        if (resp.resp !== "ok") {
          throw new Error(`set_config failed: ${JSON.stringify(resp)}`);
        }
        // Keep the in-process close-to-tray sentinel in sync so the change
        // takes effect without a restart (issue #31). The daemon persisted it
        // above; this updates the synchronous close-button handler's mirror.
        if (toSend.close_to_tray !== null) {
          await invoke("set_close_to_tray", { enabled: toSend.close_to_tray });
        }
        resolve();
      } catch (e) {
        // Roll back to authoritative state on error.
        _state.set({ snapshot: null, fetchedAt: 0 });
        await fetchConfig().catch(() => {});
        reject(e);
      }
    }, 500);
  });
}

// ---------------------------------------------------------------------------
// IPC helper functions for 2.F Command surface
// ---------------------------------------------------------------------------

/** Change the vault passphrase. Throws on IPC error. */
export async function changePassphrase(oldPass: string, newPass: string): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "change_passphrase",
    old: oldPass,
    new: newPass,
  });
  const result = unwrapOk(resp);
  if (result.result !== "passphrase_changed") {
    throw new Error(`unexpected reply: ${result.result}`);
  }
}

/** Mute or unmute notifications for a contact. */
export async function setContactMuted(contact: string, muted: boolean): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "set_contact_muted",
    contact,
    muted,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error(`set_contact_muted failed: ${JSON.stringify(resp)}`);
  }
}

/**
 * Tail the in-memory log ring buffer.
 * Pass `sinceSeq = null` to start from the oldest buffered record.
 */
export async function tailLogs(
  sinceSeq: bigint | null,
  limit: number,
): Promise<{ records: LogRecord[]; nextSinceSeq: bigint }> {
  const resp = await ipcClient.request({
    cmd: "tail_logs",
    since_seq: sinceSeq,
    limit,
  });
  const result = unwrapOk(resp);
  if (result.result !== "logs") {
    throw new Error(`unexpected reply: ${result.result}`);
  }
  return { records: result.data.records, nextSinceSeq: result.data.next_since_seq };
}

/**
 * Returns the Unix timestamp (seconds) when the passphrase was last changed,
 * or `null` if it has never been changed since identity init.
 */
export async function getPassphraseAuditLatest(): Promise<bigint | null> {
  const resp = await ipcClient.request({ cmd: "get_passphrase_audit_latest" });
  const result = unwrapOk(resp);
  if (result.result !== "passphrase_audit") {
    throw new Error(`unexpected reply: ${result.result}`);
  }
  return result.data.last_changed_unix;
}

/**
 * Wipe all persistent data and shut down the daemon.
 * IRREVERSIBLE — the caller must confirm with the user first.
 */
export async function wipeAllData(): Promise<void> {
  const resp = await ipcClient.request({ cmd: "wipe_all_data" });
  if (resp.resp !== "ok") {
    throw new Error(`wipe_all_data failed: ${JSON.stringify(resp)}`);
  }
}

/**
 * Export an encrypted backup of the database to the given path.
 * The file is written by the daemon as an age-encrypted `.age` archive.
 */
export async function exportBackup(destPath: string): Promise<void> {
  const resp = await ipcClient.request({ cmd: "export_backup", dest_path: destPath });
  if (resp.resp === "err") {
    throw new Error(errorMessage(resp.data));
  } else if (resp.resp !== "ok") {
    throw new Error(`export_backup: unexpected response ${resp.resp}`);
  }
}
