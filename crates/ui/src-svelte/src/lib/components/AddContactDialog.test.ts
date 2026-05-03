// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import AddContactDialog from "./AddContactDialog.svelte";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: { result: "contact_added", data: {} },
    }),
  },
}));

vi.mock("$lib/stores/contacts", () => ({
  refreshContacts: vi.fn(),
}));

import { ipcClient } from "$lib/ipc/tauri";

describe("AddContactDialog (paste tab)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("submits AddContact with the pasted URL", async () => {
    const onClose = vi.fn();
    const { getByPlaceholderText, getByText } = render(AddContactDialog, {
      props: { onClose },
    });
    const input = getByPlaceholderText(/skattr:\/\/invite/i) as HTMLTextAreaElement;
    await fireEvent.input(input, {
      target: { value: "skattr://invite/v1#abc" },
    });
    await fireEvent.click(getByText("Add contact"));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "add_contact",
      invite_url: "skattr://invite/v1#abc",
    });
  });
});
