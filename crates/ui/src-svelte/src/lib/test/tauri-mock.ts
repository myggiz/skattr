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

// Pre-seed a contact + handle send_message when ?fixture=seeded-contact.
const _fixtureSeeded =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "seeded-contact";

// Pre-seed a 200-message conversation when ?fixture=seeded-200-msgs.
const _fixture200Msgs =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "seeded-200-msgs";

let _vault = _preseeded || _fixtureSeeded || _fixture200Msgs;

// Active subscribe channel — used by fixture to emit delivery_status_changed.
let _subscribeChannel: Channel<unknown> | null = null;

const MOCK_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon art";

const MOCK_ONION = "abcd1234efgh5678abcd1234efgh5678abcd1234efgh5678.onion";
const MOCK_PUBKEY = "00".repeat(32);

// ---------------------------------------------------------------------------
// Fixture: seeded contact
// ---------------------------------------------------------------------------
const FIXTURE_PEER_PUBKEY = "ab".repeat(32);  // deterministic fake peer pubkey
const FIXTURE_MESSAGE_ID = "cd".repeat(16);    // deterministic fake message_id

// ---------------------------------------------------------------------------
// Fixture: seeded-200-msgs — 200-message conversation for pagination tests
// ---------------------------------------------------------------------------
const SEED_PEER_PUBKEY = "ef".repeat(32);  // deterministic fake peer pubkey for pagination fixture

function seededMessages(): Array<{
  row_id: bigint;
  message_id: string;
  contact: string;
  direction: string;
  kind: { kind: string; body: string };
  mls_generation: bigint;
  ts_daemon_recv: bigint;
  ts_envelope: bigint;
}> {
  return Array.from({ length: 200 }, (_, i) => ({
    row_id: BigInt(i + 1),
    message_id: (i + 1).toString(16).padStart(32, "0"),
    contact: SEED_PEER_PUBKEY,
    direction: "incoming",
    kind: { kind: "text", body: `msg ${i + 1}` },
    mls_generation: 0n,
    ts_daemon_recv: BigInt(1_700_000_000 + i),
    ts_envelope: BigInt(1_700_000_000 + i),
  }));
}

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
        let contactList: unknown[] = [];
        if (_fixtureSeeded) {
          contactList = [
            {
              pubkey: FIXTURE_PEER_PUBKEY,
              nickname: "test peer",
              onion: "fakeonion.onion",
              card_version: 1,
              added_at: 0,
              unread_count: 0,
              last_message_preview: null,
              last_ts_recv: null,
              group_state: "active",
              last_read_row_id: null,
            },
          ];
        } else if (_fixture200Msgs) {
          contactList = [
            {
              pubkey: SEED_PEER_PUBKEY,
              nickname: "pagination peer",
              onion: "paginationonion.onion",
              card_version: 1,
              added_at: 0,
              unread_count: 0,
              last_message_preview: null,
              last_ts_recv: null,
              group_state: "active",
              last_read_row_id: null,
            },
          ];
        }
        return {
          resp: "ok",
          data: { result: "contacts", data: contactList },
        } as unknown as T;
      }
      if (cmdObj.cmd === "recent_messages") {
        if (_fixture200Msgs) {
          const msgCmd = cmdObj as { cmd: string; before_id: bigint | null; limit: number };
          const all = seededMessages();
          const cursorNum = msgCmd.before_id !== null && msgCmd.before_id !== undefined
            ? Number(msgCmd.before_id)
            : Number.MAX_SAFE_INTEGER;
          const filtered = all.filter((m) => Number(m.row_id) < cursorNum);
          // Sort DESC (most-recent-first) to match daemon behaviour.
          const sorted = filtered.sort((a, b) => Number(b.row_id) - Number(a.row_id));
          const page = sorted.slice(0, 50);
          const nextBeforeId = page.length === 50 ? page[page.length - 1].row_id : null;
          // Artificial 80 ms delay so Playwright can see intermediate page counts
          // without cascading all 4 pages in a single tick.
          await new Promise<void>((resolve) => setTimeout(resolve, 80));
          return {
            resp: "ok",
            data: {
              result: "messages_page",
              data: { records: page, next_before_id: nextBeforeId },
            },
          } as unknown as T;
        }
        return {
          resp: "ok",
          data: {
            result: "messages_page",
            data: { records: [], next_before_id: null },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "mark_read") {
        return {
          resp: "ok",
          data: { result: "marked_read", data: { up_to: 0 } },
        } as unknown as T;
      }
      if (cmdObj.cmd === "send_message") {
        const msgCmd = cmdObj as { cmd: string; contact: string; kind: unknown };
        const record = {
          row_id: 1,
          message_id: FIXTURE_MESSAGE_ID,
          contact: msgCmd.contact,
          direction: "outgoing",
          kind: msgCmd.kind,
          mls_generation: 1,
          ts_daemon_recv: Math.floor(Date.now() / 1000),
          ts_envelope: Date.now(),
        };
        // Schedule a delivery_status_changed event 200 ms after the send,
        // simulating the daemon advancing from Queued → Delivered.
        setTimeout(() => {
          if (_subscribeChannel) {
            _subscribeChannel._emit({
              event: "delivery_status_changed",
              data: { message: FIXTURE_MESSAGE_ID, status: "Delivered" },
            });
          }
        }, 200);
        return {
          resp: "ok",
          data: {
            result: "message_sent",
            data: {
              message_id: FIXTURE_MESSAGE_ID,
              status: "queued",
              record,
            },
          },
        } as unknown as T;
      }
      throw new Error(`ipc_request: no mock for cmd=${cmdObj.cmd}`);
    }

    case "ipc_subscribe": {
      const channel = args?.channel as Channel<unknown> | undefined;
      if (!channel) throw new Error("ipc_subscribe: missing channel arg");
      // Save channel ref so fixture send_message can emit delivery events.
      _subscribeChannel = channel;
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
