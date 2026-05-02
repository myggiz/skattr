// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import { flushSync } from "svelte";
import Composer from "./Composer.svelte";
import { ipcClient } from "$lib/ipc/tauri";
import type { PublicKey } from "$lib/ipc/types";
import { conversation } from "$lib/stores/conversation";

const peer: PublicKey = "0707070707070707070707070707070707070707070707070707070707070707";

beforeEach(() => {
  conversation.set({
    contact: peer,
    messages: [],
    nextBeforeId: null,
    loadingOlder: false,
    unreadAnchorRowId: null,
    readCursor: 0n,
  });
  vi.spyOn(ipcClient, "request").mockResolvedValue({
    resp: "ok",
    data: {
      result: "message_sent",
      data: {
        message_id: "00000000000000000000000000000000",
        status: "Queued",
        record: null,
      },
    },
  } as any);
});

describe("Composer", () => {
  test("Enter sends when not composing", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "hi" } });
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: false });
    expect(ipcClient.request).toHaveBeenCalledWith(
      expect.objectContaining({
        cmd: "send_message",
        kind: { kind: "text", body: "hi" },
      }),
    );
  });

  test("Enter no-ops while IME composition is active", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "x" } });
    await fireEvent.compositionStart(ta);
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: true });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("Shift+Enter does not send", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "hi" } });
    await fireEvent.keyDown(ta, { key: "Enter", shiftKey: true, isComposing: false });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("Empty / whitespace input + Enter no-ops", async () => {
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    await fireEvent.input(ta, { target: { value: "   " } });
    await fireEvent.keyDown(ta, { key: "Enter", isComposing: false });
    expect(ipcClient.request).not.toHaveBeenCalled();
  });

  test("paste inserts only text/plain", async () => {
    // jsdom doesn't implement DataTransfer, so we dispatch a custom paste event
    // with a hand-mocked clipboardData that only exposes text/plain.
    const { getByRole } = render(Composer, { props: { contact: peer, disabled: false } });
    const ta = getByRole("textbox") as HTMLTextAreaElement;
    const mockClipboardData = {
      getData: (type: string) => (type === "text/plain" ? "bold" : "<b>bold</b>"),
    };
    const paste = new Event("paste", { bubbles: true }) as ClipboardEvent;
    Object.defineProperty(paste, "clipboardData", { value: mockClipboardData });
    ta.dispatchEvent(paste);
    // Svelte 5 batches reactive state updates; flushSync forces them to DOM.
    flushSync();
    expect(ta.value).not.toContain("<b>");
    expect(ta.value).toBe("bold");
  });

  test("disabled prop disables textarea + send button", () => {
    const { getByRole } = render(Composer, {
      props: { contact: peer, disabled: true, disabledReason: "Daemon not running" },
    });
    expect((getByRole("textbox") as HTMLTextAreaElement).disabled).toBe(true);
    expect((getByRole("button") as HTMLButtonElement).disabled).toBe(true);
  });
});
