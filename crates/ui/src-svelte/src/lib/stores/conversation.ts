// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, get } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import type { ContactSummary, MessageRecord, PublicKey } from "$lib/ipc/types";

export type OptimisticMessage = MessageRecord & {
  __tempId: string;
  __optimistic: true;
  __failed?: string;
};

interface ConversationState {
  contact: PublicKey | null;
  messages: (MessageRecord | OptimisticMessage)[];
  nextBeforeId: bigint | null;
  loadingOlder: boolean;
  unreadAnchorRowId: bigint | null;
  readCursor: bigint;
}

export const conversation = writable<ConversationState>({
  contact: null,
  messages: [],
  nextBeforeId: null,
  loadingOlder: false,
  unreadAnchorRowId: null,
  readCursor: 0n,
});

/**
 * String equality on the hex-encoded PublicKey emitted by ts-rs.
 * Kept as a named helper so the type-narrowing intent stays
 * obvious at call sites.
 */
function pubkeyEq(a: PublicKey, b: PublicKey): boolean {
  return a === b;
}

export function appendOptimistic(
  contact: PublicKey,
  body: string,
  tempId: string,
): void {
  conversation.update((state) => {
    if (state.contact === null || !pubkeyEq(state.contact, contact)) {
      return state;
    }
    const placeholder: OptimisticMessage = {
      __tempId: tempId,
      __optimistic: true,
      row_id: -1n,
      message_id: "00000000000000000000000000000000",
      contact,
      direction: "outgoing",
      kind: { kind: "text", body },
      mls_generation: 0n,
      ts_daemon_recv: BigInt(Math.floor(Date.now() / 1000)),
      ts_envelope: BigInt(Date.now()),
    };
    return { ...state, messages: [...state.messages, placeholder] };
  });
}

export function reconcile(tempId: string, canonical: MessageRecord): void {
  conversation.update((state) => {
    const idx = state.messages.findIndex(
      (m) => (m as OptimisticMessage).__tempId === tempId,
    );
    if (idx < 0) return state;
    const next = [...state.messages];
    next[idx] = canonical;
    return { ...state, messages: next };
  });
}

export function markFailed(tempId: string, reason: string): void {
  conversation.update((state) => {
    const idx = state.messages.findIndex(
      (m) => (m as OptimisticMessage).__tempId === tempId,
    );
    if (idx < 0) return state;
    const target = { ...(state.messages[idx] as OptimisticMessage), __failed: reason };
    const next = [...state.messages];
    next[idx] = target;
    return { ...state, messages: next };
  });
}

export async function openConversationFromSummary(
  summary: ContactSummary,
): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact: summary.pubkey,
    limit: 50,
    before_id: null,
    paged: true,
  });
  const result = unwrapOk(resp);
  const records: MessageRecord[] = [];
  let nextBeforeId: bigint | null = null;
  if (result.result === "messages_page") {
    records.push(...[...result.data.records].reverse());
    nextBeforeId = result.data.next_before_id ?? null;
  }
  const anchor = summary.last_read_row_id ?? null;
  conversation.set({
    contact: summary.pubkey,
    messages: records,
    nextBeforeId,
    loadingOlder: false,
    unreadAnchorRowId: anchor,
    readCursor: anchor ?? 0n,
  });

  // Mark-read for the largest row in the page so the contact-list
  // badge clears on open. Daemon is idempotent if up_to <= current.
  if (records.length > 0) {
    const maxRowId = records.reduce<bigint>(
      (acc, r) => (r.row_id > acc ? r.row_id : acc),
      0n,
    );
    if (maxRowId > 0n) {
      void ipcClient.request({
        cmd: "mark_read",
        contact: summary.pubkey,
        up_to_message_id: maxRowId,
      });
    }
  }
}

export async function loadOlder(): Promise<void> {
  const state = get(conversation);
  if (state.loadingOlder || state.nextBeforeId === null || state.contact === null) {
    return;
  }
  conversation.update((s) => ({ ...s, loadingOlder: true }));
  try {
    const resp = await ipcClient.request({
      cmd: "recent_messages",
      contact: state.contact,
      limit: 50,
      before_id: state.nextBeforeId,
      paged: true,
    });
    const result = unwrapOk(resp);
    if (result.result === "messages_page") {
      const olderChrono = [...result.data.records].reverse();
      conversation.update((s) => ({
        ...s,
        messages: [...olderChrono, ...s.messages],
        nextBeforeId: result.data.next_before_id ?? null,
        loadingOlder: false,
      }));
    } else {
      conversation.update((s) => ({ ...s, loadingOlder: false }));
    }
  } catch (e) {
    conversation.update((s) => ({ ...s, loadingOlder: false }));
    throw e;
  }
}

export function appendMessage(record: MessageRecord): void {
  conversation.update((state) => {
    if (
      state.contact !== null &&
      pubkeyEq(record.contact as PublicKey, state.contact)
    ) {
      return { ...state, messages: [...state.messages, record] };
    }
    return state;
  });
}

const MARK_READ_DEBOUNCE_MS = 500;
const BOTTOM_PROXIMITY_PX = 100;

export function isWithinBottomThreshold(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_PROXIMITY_PX;
}

let markReadTimer: ReturnType<typeof setTimeout> | null = null;
let pendingHighestRowId: bigint = 0n;

export function markReadIfAtBottom(rowId: bigint): void {
  const state = get(conversation);
  if (state.contact === null) return;
  if (rowId <= state.readCursor) return;
  if (rowId > pendingHighestRowId) pendingHighestRowId = rowId;
  if (markReadTimer) clearTimeout(markReadTimer);
  markReadTimer = setTimeout(async () => {
    const target = pendingHighestRowId;
    pendingHighestRowId = 0n;
    markReadTimer = null;
    const cur = get(conversation);
    if (cur.contact === null || target <= cur.readCursor) return;
    try {
      await ipcClient.request({
        cmd: "mark_read",
        contact: cur.contact,
        up_to_message_id: target,
      });
      conversation.update((s) => ({ ...s, readCursor: target }));
    } catch (e) {
      // Swallow per spec §4.4 — non-critical; next open retries.
      console.warn("mark_read failed:", e);
    }
  }, MARK_READ_DEBOUNCE_MS);
}
