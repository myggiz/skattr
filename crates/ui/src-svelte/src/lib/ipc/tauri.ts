// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// TauriTransport: realises the IpcClient interface over Tauri 2 IPC.

import { invoke, Channel } from "@tauri-apps/api/core";

import type { IpcClient } from "./client";
import type { Command, Event, EventFilter, IpcResponse } from "./types";

export class TauriTransport implements IpcClient {
  async request(cmd: Command): Promise<IpcResponse> {
    return await invoke<IpcResponse>("ipc_request", { cmd });
  }

  async subscribe(
    filter: EventFilter,
    onEvent: (e: Event) => void,
  ): Promise<() => void> {
    const channel = new Channel<Event>();
    channel.onmessage = onEvent;
    await invoke("ipc_subscribe", { filter, channel });
    return () => {
      // Tauri Channel doesn't have an explicit close; the Rust side's
      // `tokio::spawn` loop exits when `channel.send` fails. Drop the
      // handler so further events are ignored.
      channel.onmessage = () => {};
    };
  }
}

export const ipcClient: IpcClient = new TauriTransport();
