// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, type Writable } from "svelte/store";
import { pubkeyEq } from "$lib/pubkey";
import type { ContactSummary } from "$lib/ipc/types";

/** A contact whose first-contact Welcome is still in flight. */
export function isConnecting(c: ContactSummary): boolean {
  return c.group_state === "pending_join";
}

/** Grace window (seconds) before a still-pending first contact escalates from
 *  "Connecting…" to the honest "Not connected yet" state. First contact over
 *  Tor legitimately takes ~30–90 s. */
export const CONNECTING_GRACE_SECS = 120;

/** Display state for a not-yet-confirmed first contact (#101), or null if the
 *  contact is not pending. `nowSecs` is unix seconds; `added_at` is when first
 *  contact started (unix seconds), so `now − added_at` is how long it's been
 *  pending. */
export function pendingState(
  c: ContactSummary,
  nowSecs: number,
): "connecting" | "unconfirmed" | "failed" | null {
  if (c.group_state !== "pending_join") return null;
  if (c.welcome_failed) return "failed";
  const elapsed = Math.max(0, nowSecs - Number(c.added_at));
  return elapsed < CONNECTING_GRACE_SECS ? "connecting" : "unconfirmed";
}

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";

export const contacts = writable<ContactSummary[]>([]);

export async function refreshContacts(): Promise<void> {
  const resp = await ipcClient.request({ cmd: "list_contacts" });
  const result = unwrapOk(resp);
  // CommandResult shape from ts-rs: { result: "contacts", data: ContactSummary[] }
  if (result.result === "contacts") {
    contacts.set(result.data);
  }
}

/** Remove a contact from the store by pubkey (hex). Used on the
 *  `contact_removed` event so the row disappears without a full refetch. */
export function dropContact(pubkey: string): void {
  contacts.update((cs) => cs.filter((c) => !pubkeyEq(c.pubkey, pubkey)));
}

export const expandedPubkey: Writable<string | null> = writable(null);

export function toggleExpanded(pubkey: string): void {
  expandedPubkey.update((current) => (pubkeyEq(current, pubkey) ? null : pubkey));
}

export async function rename(contact: string, nickname: string | null): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "rename_contact",
    contact,
    nickname,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error("rename_contact failed");
  }
  await refreshContacts();
}

/** Remove a pending contact via the daemon. The daemon hard-wipes local state
 *  (#109); the `contact_removed` event then drops it from the store, so we do
 *  not refetch here. */
export async function removeContact(contact: string): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "remove_contact",
    contact,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error("remove_contact failed");
  }
}

export async function archive(contact: string): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "remove_contact",
    contact,
  } as any);
  if (resp.resp !== "ok") {
    throw new Error("remove_contact failed");
  }
  await refreshContacts();
}
