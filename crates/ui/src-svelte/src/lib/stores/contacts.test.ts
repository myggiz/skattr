// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { isConnecting, pendingState } from "./contacts";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: { result: "contacts", data: [] },
    }),
  },
}));

import { ipcClient } from "$lib/ipc/tauri";
import { rename, archive, removeContact, toggleExpanded, expandedPubkey, contacts, dropContact } from "./contacts";
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

describe("pendingState", () => {
  it("connecting within the grace window", () => {
    expect(
      pendingState({ group_state: "pending_join", added_at: 1000n } as any, 1000 + 30),
    ).toBe("connecting");
  });
  it("unconfirmed after the grace window", () => {
    expect(
      pendingState({ group_state: "pending_join", added_at: 1000n } as any, 1000 + 200),
    ).toBe("unconfirmed");
  });
  it("null for an active contact", () => {
    expect(pendingState({ group_state: "active", added_at: 0n } as any, 99999)).toBeNull();
  });
  it("clamps negative elapsed to connecting", () => {
    expect(
      pendingState({ group_state: "pending_join", added_at: 5000n } as any, 1000),
    ).toBe("connecting");
  });
  it("returns 'failed' when welcome_failed is true, regardless of elapsed", () => {
    const c = { group_state: "pending_join", added_at: 0, welcome_failed: true } as any;
    expect(pendingState(c, 5)).toBe("failed");
    expect(pendingState(c, 10_000)).toBe("failed");
  });
  it("unaffected when welcome_failed is false", () => {
    const c = { group_state: "pending_join", added_at: 0, welcome_failed: false } as any;
    expect(pendingState(c, 5)).toBe("connecting");
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

  it("removeContact issues remove_contact for the pubkey without refreshing", async () => {
    vi.clearAllMocks();
    await removeContact("aa".repeat(32));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "remove_contact",
      contact: "aa".repeat(32),
    });
    // Must NOT call list_contacts — the contact_removed event drives the store drop
    expect(ipcClient.request).not.toHaveBeenCalledWith({ cmd: "list_contacts" });
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

describe("dropContact", () => {
  it("removes the matching contact by pubkey", () => {
    contacts.set([
      { pubkey: "aa", nickname: "A" } as any,
      { pubkey: "bb", nickname: "B" } as any,
    ]);
    dropContact("aa");
    expect(get(contacts).map((c: any) => c.pubkey)).toEqual(["bb"]);
  });
});
