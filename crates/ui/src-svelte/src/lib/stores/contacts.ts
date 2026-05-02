// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { ContactSummary } from "$lib/ipc/types";

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
