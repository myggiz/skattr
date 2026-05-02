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
