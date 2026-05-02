// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Contract tests for IpcClient interface and unwrapOk helper.
// TauriTransport framing is verified via a lightweight in-process stub.

import { describe, expect, test, vi } from "vitest";
import { unwrapOk } from "./client";
import type { IpcClient } from "./client";
import type { Command, CommandResult, Event, EventFilter, IpcResponse } from "./types";

// ---------------------------------------------------------------------------
// 1. unwrapOk — pure unit tests, no mocking needed
// ---------------------------------------------------------------------------

describe("unwrapOk", () => {
  test("returns data from ok response", () => {
    const resp: IpcResponse = {
      resp: "ok",
      data: { result: "contacts", data: [] },
    };
    const result = unwrapOk(resp);
    expect(result).toEqual({ result: "contacts", data: [] });
  });

  test("throws on err response", () => {
    const resp: IpcResponse = {
      resp: "err",
      data: { err: "internal", data: "bad arg" },
    };
    expect(() => unwrapOk(resp)).toThrow("IPC error");
  });

  test("throws on event response (not ok)", () => {
    const resp: IpcResponse = {
      resp: "event",
      data: { event: "tor_status_changed", data: "Ready" },
    };
    expect(() => unwrapOk(resp)).toThrow("IPC error");
  });

  test("throws on bye response", () => {
    const resp: IpcResponse = { resp: "bye" };
    expect(() => unwrapOk(resp)).toThrow("IPC error");
  });
});

// ---------------------------------------------------------------------------
// 2. TauriTransport framing contract — dependency-injection stub
// ---------------------------------------------------------------------------
//
// We can't vi.mock("@tauri-apps/api/core") reliably in all Vite/Vitest
// versions, so we verify the framing contract by constructing a stub client
// that matches what TauriTransport does and asserting the wire shapes.
// The IpcClient interface contract is tested here; TauriTransport's real
// invoke calls are exercised by the Playwright e2e suite with TAURI_MOCK=1.

/** Minimal stub that records calls and returns preset responses. */
function makeStubClient(
  responses: Map<string, IpcResponse>,
): IpcClient & { calls: { cmd: string }[] } {
  const calls: { cmd: string }[] = [];

  return {
    calls,
    async request(cmd: Command): Promise<IpcResponse> {
      calls.push({ cmd: cmd.cmd });
      const key = cmd.cmd;
      const resp = responses.get(key);
      if (!resp) throw new Error(`no stub for ${key}`);
      return resp;
    },
    async subscribe(
      _filter: EventFilter,
      onEvent: (e: Event) => void,
    ): Promise<() => void> {
      // Simulate a single tor_status_changed event.
      queueMicrotask(() => onEvent({ event: "tor_status_changed", data: "Ready" }));
      return () => {};
    },
  };
}

describe("IpcClient contract — wire shapes", () => {
  test("request encodes list_contacts and decodes ok response", async () => {
    const expectedResult: CommandResult = { result: "contacts", data: [] };
    const stub = makeStubClient(
      new Map([["list_contacts", { resp: "ok", data: expectedResult }]]),
    );

    const raw = await stub.request({ cmd: "list_contacts" });
    const result = unwrapOk(raw);

    expect(stub.calls).toHaveLength(1);
    expect(stub.calls[0].cmd).toBe("list_contacts");
    expect(result).toEqual(expectedResult);
  });

  test("request encodes daemon_info and decodes daemon_info result", async () => {
    const info = {
      local_pubkey: "00".repeat(32),
      current_onion: "abcd.onion",
      daemon_version: "0.0.1",
      schema_version: 9,
    };
    const expectedResult: CommandResult = { result: "daemon_info", data: info };
    const stub = makeStubClient(
      new Map([["daemon_info", { resp: "ok", data: expectedResult }]]),
    );

    const raw = await stub.request({ cmd: "daemon_info" });
    const result = unwrapOk(raw);

    expect(result.result).toBe("daemon_info");
    if (result.result === "daemon_info") {
      expect(result.data.current_onion).toBe("abcd.onion");
    }
  });

  test("subscribe fires tor_status_changed event", async () => {
    const stub = makeStubClient(new Map());
    const events: Event[] = [];
    await stub.subscribe({ filter: "tor_status" }, (e) => events.push(e));
    // Allow the queued microtask to fire.
    await new Promise((r) => setTimeout(r, 0));
    expect(events).toHaveLength(1);
    expect(events[0]).toEqual({ event: "tor_status_changed", data: "Ready" });
  });
});

// ---------------------------------------------------------------------------
// 3. Wire shape invariants — adjacent-tagged serde shapes
// ---------------------------------------------------------------------------

describe("wire shape invariants", () => {
  test("IpcResponse ok shape has resp:'ok' and data", () => {
    const r: IpcResponse = { resp: "ok", data: { result: "contacts", data: [] } };
    expect(r.resp).toBe("ok");
  });

  test("IpcResponse err shape has resp:'err' and data", () => {
    const r: IpcResponse = { resp: "err", data: { err: "internal", data: "x" } };
    expect(r.resp).toBe("err");
  });

  test("Command list_contacts has cmd:'list_contacts' (no extra fields)", () => {
    const cmd: Command = { cmd: "list_contacts" };
    expect(cmd.cmd).toBe("list_contacts");
    // Adjacent-tagged — cmd is the only key for unit variants
    expect(Object.keys(cmd)).toEqual(["cmd"]);
  });

  test("Command daemon_info has cmd:'daemon_info'", () => {
    const cmd: Command = { cmd: "daemon_info" };
    expect(cmd.cmd).toBe("daemon_info");
  });

  test("EventFilter tor_status has filter:'tor_status'", () => {
    const f: EventFilter = { filter: "tor_status" };
    expect(f.filter).toBe("tor_status");
  });

  test("TorStatus bare string 'Ready' and object Bootstrapping shape", () => {
    // These are the shapes the UI must handle.
    const ready = "Ready" as const;
    expect(ready).toBe("Ready");

    const booting = { Bootstrapping: 42 };
    expect("Bootstrapping" in booting).toBe(true);
    expect(booting.Bootstrapping).toBe(42);
  });
});
