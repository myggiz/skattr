// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";
import { isConnecting } from "./contacts";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: { result: "contacts", data: [] },
    }),
  },
}));

import { ipcClient } from "$lib/ipc/tauri";
import { rename, archive, toggleExpanded, expandedPubkey } from "./contacts";
import { get } from "svelte/store";

describe("isConnecting", () => {
  it("true while group_state is pending_join", () => {
    expect(isConnecting({ group_state: "pending_join" } as any)).toBe(true);
  });
  it("false when active", () => {
    expect(isConnecting({ group_state: "active" } as any)).toBe(false);
  });
  it("false when null", () => {
    expect(isConnecting({ group_state: null } as any)).toBe(false);
  });
});

describe("contacts store", () => {
  beforeEach(() => vi.clearAllMocks());

  it("rename calls Command::RenameContact and refreshes", async () => {
    await rename("aa".repeat(32), "Alice");
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "rename_contact",
      contact: "aa".repeat(32),
      nickname: "Alice",
    });
    expect(ipcClient.request).toHaveBeenCalledWith({ cmd: "list_contacts" });
  });

  it("archive calls Command::RemoveContact and refreshes", async () => {
    await archive("bb".repeat(32));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "remove_contact",
      contact: "bb".repeat(32),
    });
  });

  it("toggleExpanded enforces single-select", () => {
    toggleExpanded("aa");
    expect(get(expandedPubkey)).toBe("aa");
    toggleExpanded("bb");
    expect(get(expandedPubkey)).toBe("bb");
    toggleExpanded("bb");
    expect(get(expandedPubkey)).toBeNull();
  });
});
