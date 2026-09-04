// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { writable, get } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import { pubkeyEq } from "$lib/pubkey";
import type { ContactSummary, Hex16, MessageRecord, PublicKey } from "$lib/ipc/types";

export type OptimisticMessage = MessageRecord & {
  __tempId: string;
  __optimistic: true;
  __failed?: string;
  __attachName?: string;
  __attachSize?: number;
  /**
   * Hex attachment id, learned from FileQueued and attached at promotion.
   * The placeholder's manifest is empty, so the bubble cannot decode the id
   * the way it does for a real record — without this it reads the transfer
   * store under `null` and never reflects the transfer (#177).
   */
  __attachId?: string;
};

/**
 * An optimistic message that has been promoted to non-optimistic after the
 * daemon acknowledged it (FileQueued / message_sent), while still carrying the
 * optimistic display fields until the canonical MessageRecord arrives. Narrows
 * `__optimistic` to `false` so the promotion needs no `as unknown` cast.
 */
export type PromotedMessage = Omit<OptimisticMessage, "__optimistic"> & {
  __optimistic: false;
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
 * #178: collapse back to the no-conversation state.
 *
 * Clears the loaded messages as well as the active contact — the point of the
 * toggle is that a user can leave the app open with no conversation content on
 * screen, so leaving the array populated would defeat it.
 *
 * Purely local: no IPC, and in particular no read-state write, so closing a
 * conversation never marks anything read that the user did not read.
 */
/**
 * Monotonic load token. A conversation load is async, so its response can land
 * after the user has already closed the conversation or picked another contact.
 * Every call that establishes conversation state claims a fresh token; a load
 * only commits its response if its token is still the current one. Without
 * this, a close that lands mid-load is silently undone when the response
 * arrives (found reviewing #178).
 */
let loadToken = 0;

export function closeConversation(): void {
  loadToken++;
  conversation.set({
    contact: null,
    messages: [],
    nextBeforeId: null,
    loadingOlder: false,
    unreadAnchorRowId: null,
    readCursor: 0n,
  });
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
      delivered_at: null,
      dismissed_at: null,
      failed_reason: null,
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

/**
 * Seed the delivery store from persisted history (#200, extended for the
 * outbox-delivery-truthfulness workstream).
 *
 * The store is session-scoped, so without this a reloaded conversation shows
 * nothing for messages the daemon has recorded as delivered or given up on —
 * which is how every sent message came back as "still in flight" after a
 * restart. Delivered wins over a `failed_reason`: an ack is ground truth,
 * while a stored failure reason could be stale from an earlier attempt that
 * later succeeded. Exported so tests can hydrate the delivery store directly
 * without a full `openConversationFromSummary` round trip.
 */
export function hydrateDeliveryFromRecords(records: MessageRecord[]): void {
  for (const r of records) {
    if (r.direction === "outgoing") {
      const hex = hex16ToString(r.message_id);
      if (r.delivered_at !== null && r.delivered_at !== undefined) {
        recordDeliveryStatus(hex, "Delivered");
      } else if (r.failed_reason !== null && r.failed_reason !== undefined) {
        // Durable: the daemon stored the reason when it gave up, so the
        // remedy survives a restart rather than coming back as a bare clock.
        recordDeliveryStatus(hex, { Failed: r.failed_reason });
      }
    }
  }
}

export async function openConversationFromSummary(
  summary: ContactSummary,
): Promise<void> {
  const token = ++loadToken;
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact: summary.pubkey,
    limit: 50,
    before_id: null,
    paged: true,
  });
  // Superseded while in flight — by a close, or by a later selection whose
  // response may well arrive first. Drop this one rather than overwrite.
  if (token !== loadToken) return;
  const result = unwrapOk(resp);
  const records: MessageRecord[] = [];
  let nextBeforeId: bigint | null = null;
  if (result.result === "messages_page") {
    records.push(...[...result.data.records].reverse());
    nextBeforeId = result.data.next_before_id ?? null;
  }
  hydrateDeliveryFromRecords(records);
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
  const reqContact = state.contact;
  conversation.update((s) => ({ ...s, loadingOlder: true }));
  try {
    const resp = await ipcClient.request({
      cmd: "recent_messages",
      contact: reqContact,
      limit: 50,
      before_id: state.nextBeforeId,
      paged: true,
    });
    const result = unwrapOk(resp);
    if (result.result === "messages_page") {
      const olderChrono = [...result.data.records].reverse();
      hydrateDeliveryFromRecords(olderChrono);
      conversation.update((s) => {
        if (!pubkeyEq(s.contact, reqContact)) return s;
        return {
          ...s,
          messages: [...olderChrono, ...s.messages],
          nextBeforeId: result.data.next_before_id ?? null,
          loadingOlder: false,
        };
      });
    } else {
      conversation.update((s) => {
        if (!pubkeyEq(s.contact, reqContact)) return s;
        return { ...s, loadingOlder: false };
      });
    }
  } catch (e) {
    conversation.update((s) => {
      if (!pubkeyEq(s.contact, reqContact)) return s;
      return { ...s, loadingOlder: false };
    });
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
let pendingContact: PublicKey | null = null;

import { recordDeliveryStatus, hex16ToString } from "./delivery";
import { markQueued } from "./attachments";

export async function send(contact: PublicKey, body: string): Promise<void> {
  const tempId = crypto.randomUUID();
  appendOptimistic(contact, body, tempId);
  try {
    const resp = await ipcClient.request({
      cmd: "send_message",
      contact,
      kind: { kind: "text", body },
    });
    const cur = get(conversation);
    if (!pubkeyEq(cur.contact, contact)) {
      // User switched away — the daemon persisted the row;
      // it'll show up next time the original conversation is opened.
      return;
    }
    const result = unwrapOk(resp);
    if (result.result !== "message_sent") {
      markFailed(tempId, "unexpected reply variant");
      return;
    }
    const { message_id, status, record } = result.data;
    if (record) {
      reconcile(tempId, record);
      recordDeliveryStatus(
        hex16ToString(message_id),
        status === "delivered"
          ? "Delivered"
          : "Queued",
      );
    } else {
      // Idempotent retry — promote optimistic to canonical without
      // resorting to the failure flag.
      conversation.update((s) => {
        const idx = s.messages.findIndex(
          (m) => (m as OptimisticMessage).__tempId === tempId,
        );
        if (idx < 0) return s;
        const next = [...s.messages];
        const original = next[idx] as OptimisticMessage;
        next[idx] = { ...original, __optimistic: false } as MessageRecord;
        return { ...s, messages: next };
      });
    }
  } catch (e) {
    const cur = get(conversation);
    if (!pubkeyEq(cur.contact, contact)) return;
    markFailed(tempId, e instanceof Error ? e.message : String(e));
  }
}

/**
 * Optimistically insert an outgoing Kind::File bubble, issue SendFile, and
 * reconcile on FileQueued. The sender never receives download progress
 * (pull/deposit model), so we record the manifest message's delivery status
 * only; the attachments store entry stays "queued".
 */
export async function sendFile(
  contact: PublicKey,
  path: string,
  filename: string,
  size: number,
): Promise<void> {
  const tempId = crypto.randomUUID();
  conversation.update((state) => {
    if (state.contact === null || !pubkeyEq(state.contact, contact)) return state;
    const placeholder: OptimisticMessage = {
      __tempId: tempId,
      __optimistic: true,
      __attachName: filename,
      __attachSize: size,
      row_id: -1n,
      message_id: "00000000000000000000000000000000",
      contact,
      direction: "outgoing",
      kind: { kind: "file", manifest: [] as unknown as string },
      mls_generation: 0n,
      ts_daemon_recv: BigInt(Math.floor(Date.now() / 1000)),
      ts_envelope: BigInt(Date.now()),
      delivered_at: null,
      dismissed_at: null,
      failed_reason: null,
    };
    return { ...state, messages: [...state.messages, placeholder] };
  });
  try {
    const resp = await ipcClient.request({ cmd: "send_file", contact, path });
    if (!pubkeyEq(get(conversation).contact, contact)) return;
    const result = unwrapOk(resp);
    if (result.result !== "file_queued") {
      markFailed(tempId, "unexpected reply variant");
      return;
    }
    const { message_id, attachment_id, total_chunks } = result.data;
    markQueued(hex16ToString(attachment_id), { filename, size, total: total_chunks });
    recordDeliveryStatus(hex16ToString(message_id), "Queued");
    // Promote the optimistic bubble to non-optimistic; keep the carried
    // display fields so the bubble still shows name/size until the real
    // MessageRecord arrives via message_received (if it does).
    conversation.update((s) => {
      const idx = s.messages.findIndex((m) => (m as OptimisticMessage).__tempId === tempId);
      if (idx < 0) return s;
      const next = [...s.messages];
      const promoted: PromotedMessage = {
        ...(next[idx] as OptimisticMessage),
        __optimistic: false,
        __attachId: hex16ToString(attachment_id),
      };
      next[idx] = promoted;
      return { ...s, messages: next };
    });
  } catch (e) {
    if (!pubkeyEq(get(conversation).contact, contact)) return;
    markFailed(tempId, e instanceof Error ? e.message : String(e));
  }
}

export function markReadIfAtBottom(rowId: bigint): void {
  const state = get(conversation);
  if (state.contact === null) return;
  if (rowId <= state.readCursor) return;
  pendingContact = state.contact;
  if (rowId > pendingHighestRowId) pendingHighestRowId = rowId;
  if (markReadTimer) clearTimeout(markReadTimer);
  markReadTimer = setTimeout(async () => {
    const target = pendingHighestRowId;
    const scheduledContact = pendingContact;
    pendingHighestRowId = 0n;
    pendingContact = null;
    markReadTimer = null;
    const cur = get(conversation);
    if (cur.contact === null || cur.contact !== scheduledContact) return;
    if (target <= cur.readCursor) return;
    try {
      await ipcClient.request({
        cmd: "mark_read",
        contact: cur.contact,
        up_to_message_id: target,
      });
      conversation.update((s) => ({ ...s, readCursor: target }));
    } catch (e) {
      console.warn("mark_read failed:", e);
    }
  }, MARK_READ_DEBOUNCE_MS);
}

/**
 * Dismiss a failed send. The row is kept server-side (`dismissed_at` is set,
 * `failed_reason` is not cleared) — dismissal only hides the bubble's actions
 * and greys it. Local state is updated optimistically to match.
 */
export async function dismiss(messageId: Hex16): Promise<void> {
  await ipcClient.request({ cmd: "dismiss_message", message_id: messageId });
  conversation.update((s) => ({
    ...s,
    messages: s.messages.map((m) =>
      m.message_id === messageId
        ? { ...m, dismissed_at: BigInt(Math.floor(Date.now() / 1000)) }
        : m,
    ),
  }));
}
