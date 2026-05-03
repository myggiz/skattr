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

describe("AddContactDialog (scan tab)", () => {
  beforeEach(() => vi.clearAllMocks());

  function mockUserMedia(stream: MediaStream | Promise<never>) {
    Object.defineProperty(navigator, "mediaDevices", {
      writable: true,
      value: {
        getUserMedia: vi.fn().mockReturnValue(stream),
      },
    });
  }

  it("requests camera permission on tab switch", async () => {
    const fakeStream = {
      getTracks: () => [{ stop: vi.fn() }] as any,
    } as unknown as MediaStream;
    mockUserMedia(fakeStream);

    const { getByText } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledWith({
      video: true,
    });
  });

  it("shows fallback when camera permission is denied", async () => {
    mockUserMedia(Promise.reject(new Error("NotAllowedError")));
    const { getByText, findByText } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    expect(await findByText(/Camera access denied/i)).toBeTruthy();
  });

  it("stops camera stream on close", async () => {
    const stopSpy = vi.fn();
    const fakeStream = {
      getTracks: () => [{ stop: stopSpy }] as any,
    } as unknown as MediaStream;
    mockUserMedia(fakeStream);

    const { getByText, unmount } = render(AddContactDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Scan"));
    await new Promise((r) => setTimeout(r, 0));
    unmount();
    expect(stopSpy).toHaveBeenCalled();
  });
});
