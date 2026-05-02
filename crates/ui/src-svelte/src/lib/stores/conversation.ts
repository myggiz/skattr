// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

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

export async function openConversation(contact: PublicKey): Promise<void> {
  // First-page fetch using the new paged variant. Will be
  // superseded by openConversationFromSummary in Task 17 — this
  // legacy entry point stays for now to keep existing 2.C call
  // sites compiling.
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact,
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
  conversation.set({
    contact,
    messages: records,
    nextBeforeId,
    loadingOlder: false,
    unreadAnchorRowId: null,
    readCursor: 0n,
  });
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
