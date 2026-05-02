// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Deterministic Tauri 2 mock for Playwright e2e tests (TAURI_MOCK=1).
// Replaces @tauri-apps/api/core via Vite alias.
// No network calls, no CDN deps.

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// Pre-seed vault=true when URL has ?vault=yes (for unlock-path tests).
const _preseeded =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("vault") === "yes";

let _vault = _preseeded;

const MOCK_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon art";

const MOCK_ONION = "abcd1234efgh5678abcd1234efgh5678abcd1234efgh5678.onion";
const MOCK_PUBKEY = "00".repeat(32);

// ---------------------------------------------------------------------------
// Channel stub (mirrors Tauri 2 Channel<T> interface)
// ---------------------------------------------------------------------------

export class Channel<T> {
  onmessage: ((msg: T) => void) | null = null;
  /** Emit a message into the channel (used by the mock invoke internals). */
  _emit(msg: T) {
    if (this.onmessage) this.onmessage(msg);
  }
}

// ---------------------------------------------------------------------------
// invoke stub
// ---------------------------------------------------------------------------

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  switch (cmd) {
    case "vault_exists":
      return _vault as unknown as T;

    case "identity_init": {
      _vault = true;
      return { mnemonic: MOCK_MNEMONIC } as unknown as T;
    }

    case "vault_unlock": {
      if (!_vault) throw new Error("vault not found");
      return undefined as unknown as T;
    }

    case "start_in_process_cmd": {
      return {
        onion: MOCK_ONION,
        ipc_socket: "/tmp/skattr-mock.sock",
      } as unknown as T;
    }

    case "ipc_request": {
      const cmdObj = args?.cmd as { cmd: string } | undefined;
      if (!cmdObj) throw new Error("ipc_request: missing cmd arg");
      if (cmdObj.cmd === "daemon_info") {
        return {
          resp: "ok",
          data: {
            result: "daemon_info",
            data: {
              local_pubkey: MOCK_PUBKEY,
              current_onion: MOCK_ONION,
              daemon_version: "0.0.1",
              schema_version: 9,
            },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "list_contacts") {
        return {
          resp: "ok",
          data: { result: "contacts", data: [] },
        } as unknown as T;
      }
      throw new Error(`ipc_request: no mock for cmd=${cmdObj.cmd}`);
    }

    case "ipc_subscribe": {
      const channel = args?.channel as Channel<unknown> | undefined;
      if (!channel) throw new Error("ipc_subscribe: missing channel arg");
      // Fire a synthetic TorStatus Ready event after a tick so Bootstrap.svelte
      // can paint the progress bar at least once before the transition.
      setTimeout(() => {
        channel._emit({ event: "tor_status_changed", data: "Ready" });
      }, 50);
      return undefined as unknown as T;
    }

    default:
      throw new Error(`tauri-mock: unhandled invoke cmd="${cmd}"`);
  }
}
