// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
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

// Activate invite-generation mock when ?fixture=invite-flow.
const _fixtureInviteFlow =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "invite-flow";

// Activate add-contact mock when ?fixture=add-contact-flow.
const _fixtureAddContactFlow =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "add-contact-flow";

// Activate attachment mock when ?fixture=attachments.
const _fixtureAttachments =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fixture") === "attachments";

// When ?pick=huge the dialog picker returns a huge path so the size gate fires.
const _pickHuge =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("pick") === "huge";

// When ?fail=1 the send_file arm emits attachment_failed instead of received.
const _failAttachment =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("fail") === "1";

let _vault = _preseeded || _fixtureSeeded || _fixture200Msgs || _fixtureInviteFlow || _fixtureAddContactFlow || _fixtureAttachments;

// Daemon-running state, consulted by the main shell's onMount (`+page.svelte`):
// a vault existing isn't enough — the shell only stays put if the in-process
// daemon started this session. Fixtures that drive the app straight to "/"
// represent an already-running session, so seed it from `_vault`; the
// first-run / unlock flows flip it true once `start_in_process_cmd` runs.
let _daemonRunning = _vault;

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
      _daemonRunning = true;
      return {
        onion: MOCK_ONION,
        ipc_socket: "/tmp/skattr-mock.sock",
      } as unknown as T;
    }

    case "daemon_running":
      return _daemonRunning as unknown as T;

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
        } else if (_fixtureAttachments) {
          contactList = [
            {
              pubkey: FIXTURE_PEER_PUBKEY,
              nickname: "attach peer",
              onion: "attachonion.onion",
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
      if (cmdObj.cmd === "create_invite" && _fixtureInviteFlow) {
        return {
          resp: "ok",
          data: {
            result: "invite_created",
            data: {
              url: "skattr://invite/v1#fixture",
              key_package_id: "0".repeat(64),
              expires_at: 1_700_010_000,
            },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "add_contact" && _fixtureAddContactFlow) {
        return {
          resp: "ok",
          data: {
            result: "contact_added",
            data: {
              pubkey: "ab".repeat(32),
              nickname: "Fixture Peer",
              onion: "fixture.onion",
              card_version: 1,
              added_at: 0,
              unread_count: 0,
              last_message_preview: null,
              last_ts_recv: null,
              group_state: "active",
              last_read_row_id: null,
            },
          },
        } as unknown as T;
      }
      if (cmdObj.cmd === "rename_contact") {
        return { resp: "ok", data: { result: "ok", data: null } } as unknown as T;
      }
      if (cmdObj.cmd === "remove_contact") {
        return { resp: "ok", data: { result: "ok", data: null } } as unknown as T;
      }
      if (cmdObj.cmd === "send_file") {
        const fileCmd = cmdObj as { cmd: string; contact: string; path: string };
        // Emit an incoming Kind::File message + progress + received so the
        // e2e can assert the receive path too (sender-side has no progress).
        // When ?fail=1 emit attachment_failed instead of progress+received.
        setTimeout(() => {
          if (!_subscribeChannel) return;
          _subscribeChannel._emit({
            event: "message_received",
            data: {
              contact: fileCmd.contact,
              record: {
                row_id: 2, message_id: "11".repeat(16), contact: fileCmd.contact,
                direction: "incoming", kind: { kind: "file", manifest: [1, 2, 3] },
                mls_generation: 1, ts_daemon_recv: Math.floor(Date.now() / 1000),
                ts_envelope: Date.now(),
              },
            },
          });
          if (_failAttachment) {
            _subscribeChannel._emit({
              event: "attachment_failed",
              data: { attachment_id: "ab".repeat(16), reason: "transfer failed" },
            });
          } else {
            _subscribeChannel._emit({
              event: "attachment_progress", data: { attachment_id: "ab".repeat(16), received: 1, total: 2 },
            });
            _subscribeChannel._emit({
              event: "attachment_received",
              data: {
                attachment_id: "ab".repeat(16), contact: fileCmd.contact,
                filename: "photo.jpg", mime: "image/jpeg", size: 2048, path: "/dl/photo.jpg",
              },
            });
          }
        }, 100);
        return {
          resp: "ok",
          data: { result: "file_queued", data: { message_id: "22".repeat(16), attachment_id: "ab".repeat(16), total_chunks: 2 } },
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

    case "render_invite_qr":
      return "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='100'><rect width='100' height='100' fill='black'/></svg>" as unknown as T;

    case "file_size": {
      // Drive size-gate branches by filename convention.
      const p = String(args?.path ?? "");
      if (p.includes("huge")) return (200 * 1024 * 1024) as unknown as T;
      if (p.includes("big")) return (50 * 1024 * 1024) as unknown as T;
      return 2048 as unknown as T;
    }
    case "decode_attachment_manifest": {
      return {
        attachment_id: "ab".repeat(16),
        filename: "photo.jpg",
        mime: "image/jpeg",
        total_size: 2048,
      } as unknown as T;
    }
    case "open_file":
    case "reveal_in_folder":
      return undefined as unknown as T;
    case "plugin:dialog|open": {
      // @tauri-apps/plugin-dialog open() routes through invoke under this id.
      // When ?pick=huge the picker returns a path the file_size arm maps to 200 MiB.
      return (_pickHuge ? "/picked/huge.bin" : "/picked/photo.jpg") as unknown as T;
    }

    default:
      throw new Error(`tauri-mock: unhandled invoke cmd="${cmd}"`);
  }
}

export function convertFileSrc(path: string): string {
  return `asset://localhost/${path}`;
}
