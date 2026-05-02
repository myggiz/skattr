// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";

import { ipcClient } from "$lib/ipc/tauri";
import { unwrapOk } from "$lib/ipc/client";
import type { MessageRecord, PublicKey } from "$lib/ipc/types";

interface ConversationState {
  contact: PublicKey | null;
  messages: MessageRecord[];
}

export const conversation = writable<ConversationState>({
  contact: null,
  messages: [],
});

export async function openConversation(contact: PublicKey): Promise<void> {
  const resp = await ipcClient.request({
    cmd: "recent_messages",
    contact,
    limit: 200,
  });
  const result = unwrapOk(resp);
  // CommandResult shape: { result: "messages", data: MessageRecord[] }
  const messages = result.result === "messages" ? [...result.data] : [];
  // daemon returns newest-first; reverse for chronological render.
  messages.reverse();
  conversation.set({ contact, messages });
}

export function appendMessage(record: MessageRecord): void {
  conversation.update((state) => {
    if (
      state.contact !== null &&
      record.contact === state.contact
    ) {
      return { ...state, messages: [...state.messages, record] };
    }
    return state;
  });
}
