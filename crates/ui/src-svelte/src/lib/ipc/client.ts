// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Transport-agnostic IPC client. Components consume this interface
// only; the concrete `TauriTransport` lives in tauri.ts.

import type {
  Command,
  CommandResult,
  Event,
  EventFilter,
  IpcResponse,
} from "./types";

export interface IpcClient {
  /** Issue a one-shot command. Resolves with the wire response. */
  request(cmd: Command): Promise<IpcResponse>;

  /**
   * Open a long-lived event subscription matching `filter`. The
   * returned unsubscribe function closes the underlying channel.
   */
  subscribe(
    filter: EventFilter,
    onEvent: (e: Event) => void,
  ): Promise<() => void>;
}

/**
 * Convenience: extract a `CommandResult` from a successful `IpcResponse`.
 *
 * IpcResponse shape (from ts-rs adjacent-tag serde):
 *   { resp: "ok", data: CommandResult }
 *   { resp: "err", data: IpcError }
 *   { resp: "event", data: Event }
 *   { resp: "bye" }
 */
export function unwrapOk(resp: IpcResponse): CommandResult {
  if (resp.resp === "ok") return resp.data;
  throw new Error(`IPC error: ${JSON.stringify(resp)}`);
}
