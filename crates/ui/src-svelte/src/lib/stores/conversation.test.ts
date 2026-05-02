// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, beforeEach } from "vitest";
import { get } from "svelte/store";
import {
  conversation,
  appendOptimistic,
  reconcile,
  markFailed,
} from "./conversation";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

const peer: PublicKey = "0707070707070707070707070707070707070707070707070707070707070707";

beforeEach(() => {
  conversation.set({
    contact: peer,
    messages: [],
    nextBeforeId: null,
    loadingOlder: false,
    unreadAnchorRowId: null,
    readCursor: 0n,
  });
});

function fakeRecord(rowId: number, body: string): MessageRecord {
  return {
    row_id: BigInt(rowId),
    message_id: "0".repeat(32),
    contact: peer,
    direction: "outgoing",
    kind: { kind: "text", body },
    mls_generation: 1n,
    ts_daemon_recv: 100n,
    ts_envelope: 99n,
  };
}

describe("optimistic send + reconcile", () => {
  test("appendOptimistic adds a placeholder", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    const state = get(conversation);
    expect(state.messages.length).toBe(1);
    const msg: any = state.messages[0];
    expect(msg.__tempId).toBe("tmp-1");
    expect(msg.__optimistic).toBe(true);
    expect(msg.kind.body).toBe("hello");
    expect(msg.direction).toBe("outgoing");
  });

  test("reconcile replaces placeholder by tempId, preserving index", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    appendOptimistic(peer, "world", "tmp-2");
    const canonical = fakeRecord(7, "hello");
    reconcile("tmp-1", canonical);
    const state = get(conversation);
    expect(state.messages.length).toBe(2);
    expect((state.messages[0] as any).__tempId).toBeUndefined();
    expect(state.messages[0].row_id).toBe(7n);
    expect((state.messages[1] as any).__tempId).toBe("tmp-2");
  });

  test("markFailed flips placeholder to failed", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    markFailed("tmp-1", "boom");
    const state = get(conversation);
    const msg: any = state.messages[0];
    expect(msg.__failed).toBe("boom");
    expect(msg.__optimistic).toBe(true);
  });

  test("appendOptimistic on a different contact is ignored", () => {
    const other: PublicKey = "0909090909090909090909090909090909090909090909090909090909090909";
    appendOptimistic(other, "ignored", "tmp-x");
    expect(get(conversation).messages.length).toBe(0);
  });

  test("reconcile on unknown tempId is a no-op", () => {
    appendOptimistic(peer, "hello", "tmp-1");
    const before = get(conversation).messages;
    reconcile("not-a-real-tempid", fakeRecord(99, "x"));
    const after = get(conversation).messages;
    expect(after).toEqual(before);
  });
});

import { vi } from "vitest";
import { loadOlder, openConversationFromSummary, markReadIfAtBottom, isWithinBottomThreshold, send } from "./conversation";
import { statusForMessageHex } from "./delivery";
import { ipcClient } from "$lib/ipc/tauri";
import type { ContactSummary } from "$lib/ipc/types";

describe("pagination", () => {
  test("loadOlder is a no-op when nextBeforeId is null", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    const spy = vi.spyOn(ipcClient, "request");
    await loadOlder();
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });

  test("loadOlder prepends records (chronological) and updates cursor", async () => {
    conversation.set({
      contact: peer,
      messages: [fakeRecord(60, "newer")],
      nextBeforeId: 60n,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "messages_page",
        data: {
          // daemon returns most-recent-first; record ids 59, 58.
          records: [fakeRecord(59, "older1"), fakeRecord(58, "older2")],
          next_before_id: 58n,
        },
      },
    } as any);
    await loadOlder();
    const state = get(conversation);
    // Final order should be chronological: 58, 59, 60.
    expect(state.messages.map((m) => Number(m.row_id))).toEqual([58, 59, 60]);
    expect(state.nextBeforeId).toBe(58n);
    expect(state.loadingOlder).toBe(false);
  });

  test("loadOlder is idempotent under concurrent calls", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: 100n,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    let resolveFirst: (v: any) => void = () => {};
    const spy = vi.spyOn(ipcClient, "request").mockImplementationOnce(
      () => new Promise((r) => (resolveFirst = r)),
    );
    const p1 = loadOlder();
    const p2 = loadOlder(); // must short-circuit on loadingOlder flag
    resolveFirst({
      resp: "ok",
      data: { result: "messages_page", data: { records: [], next_before_id: null } },
    });
    await Promise.all([p1, p2]);
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });
});

describe("mark-read", () => {
  test("isWithinBottomThreshold true when scrolled near bottom", () => {
    const el = { scrollTop: 900, scrollHeight: 1000, clientHeight: 100 } as any;
    expect(isWithinBottomThreshold(el)).toBe(true);
  });

  test("isWithinBottomThreshold false when scrolled up", () => {
    const el = { scrollTop: 100, scrollHeight: 1000, clientHeight: 100 } as any;
    expect(isWithinBottomThreshold(el)).toBe(false);
  });

  test("markReadIfAtBottom debounces multiple bursts to a single IPC", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: peer, readCursor: 0n }));
    const spy = vi.spyOn(ipcClient, "request").mockResolvedValue({
      resp: "ok",
      data: { result: "marked_read", data: { up_to: 7n } },
    } as any);
    markReadIfAtBottom(3n);
    markReadIfAtBottom(5n);
    markReadIfAtBottom(7n);
    await vi.advanceTimersByTimeAsync(600);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith({
      cmd: "mark_read",
      contact: peer,
      up_to_message_id: 7n,
    });
    spy.mockRestore();
    vi.useRealTimers();
  });

  test("markReadIfAtBottom skips when rowId <= readCursor", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: peer, readCursor: 10n }));
    const spy = vi.spyOn(ipcClient, "request");
    markReadIfAtBottom(5n);
    await vi.advanceTimersByTimeAsync(600);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
    vi.useRealTimers();
  });

  test("markReadIfAtBottom skips when contact is null", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: null, readCursor: 0n }));
    const spy = vi.spyOn(ipcClient, "request");
    markReadIfAtBottom(99n);
    await vi.advanceTimersByTimeAsync(600);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
    vi.useRealTimers();
  });

  test("markReadIfAtBottom does not fire for a different contact after switch", async () => {
    vi.useFakeTimers();
    conversation.update((s) => ({ ...s, contact: peer, readCursor: 0n }));
    const spy = vi.spyOn(ipcClient, "request");
    markReadIfAtBottom(99n);
    // Switch contacts before the debounce fires.
    const otherPeer = "0909090909090909090909090909090909090909090909090909090909090909";
    conversation.update((s) => ({ ...s, contact: otherPeer, readCursor: 0n }));
    await vi.advanceTimersByTimeAsync(600);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
    vi.useRealTimers();
  });
});

describe("openConversationFromSummary", () => {
  test("populates unreadAnchorRowId from summary.last_read_row_id", async () => {
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "messages_page",
        data: { records: [], next_before_id: null },
      },
    } as any);
    const summary: ContactSummary = {
      pubkey: peer,
      nickname: null,
      onion: "",
      card_version: 0n,
      added_at: 0n,
      unread_count: 0n,
      last_message_preview: null,
      last_ts_recv: null,
      group_state: "active",
      last_read_row_id: 12n,
    };
    await openConversationFromSummary(summary);
    expect(get(conversation).unreadAnchorRowId).toBe(12n);
    expect(get(conversation).readCursor).toBe(12n);
    expect(get(conversation).contact).toBe(peer);
  });

  test("unreadAnchorRowId stays null when summary.last_read_row_id is null", async () => {
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "messages_page",
        data: { records: [], next_before_id: null },
      },
    } as any);
    const summary: ContactSummary = {
      pubkey: peer,
      nickname: null,
      onion: "",
      card_version: 0n,
      added_at: 0n,
      unread_count: 0n,
      last_message_preview: null,
      last_ts_recv: null,
      group_state: null,
      last_read_row_id: null,
    };
    await openConversationFromSummary(summary);
    expect(get(conversation).unreadAnchorRowId).toBeNull();
    expect(get(conversation).readCursor).toBe(0n);
  });
});

import { ipcClient } from "$lib/ipc/tauri";

describe("send status mapping", () => {
  test("inline delivered SendStatus → Delivered DeliveryStatus", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    const messageHex = "ab".repeat(16);
    const canonicalRecord = {
      row_id: 1n,
      message_id: messageHex,
      contact: peer,
      direction: "outgoing" as const,
      kind: { kind: "text" as const, body: "x" },
      mls_generation: 1n,
      ts_daemon_recv: 100n,
      ts_envelope: 99n,
    };
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "message_sent",
        data: {
          message_id: messageHex,
          status: "delivered",          // lowercase per actual wire format
          record: canonicalRecord,
        },
      },
    } as any);
    await send(peer, "x");
    expect(statusForMessageHex(messageHex)).toBe("Delivered");
  });

  test("inline queued SendStatus → Queued DeliveryStatus", async () => {
    conversation.set({
      contact: peer,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    const messageHex = "cd".repeat(16);
    vi.spyOn(ipcClient, "request").mockResolvedValueOnce({
      resp: "ok",
      data: {
        result: "message_sent",
        data: {
          message_id: messageHex,
          status: "queued",
          record: {
            row_id: 2n,
            message_id: messageHex,
            contact: peer,
            direction: "outgoing" as const,
            kind: { kind: "text" as const, body: "y" },
            mls_generation: 1n,
            ts_daemon_recv: 100n,
            ts_envelope: 99n,
          },
        },
      },
    } as any);
    await send(peer, "y");
    expect(statusForMessageHex(messageHex)).toBe("Queued");
  });
});
