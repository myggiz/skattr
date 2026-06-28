// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, type Writable } from "svelte/store";
import { pubkeyEq } from "$lib/pubkey";
import type { ContactSummary } from "$lib/ipc/types";

/** A contact whose first-contact Welcome is still in flight. */
export function isConnecting(c: ContactSummary): boolean {
  return c.group_state === "pending_join";
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
