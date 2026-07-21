// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import ContactDetailsPanel from "./ContactDetailsPanel.svelte";

const FAKE_PK = "7aa2c4d100000000000000000000000000000000000000000000000000b3e9f7";
const FAKE_ONION = "abcdefghijklmnop1234567890.onion";

vi.mock("$lib/stores/contacts", () => ({
  rename: vi.fn().mockResolvedValue(undefined),
  archive: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("$lib/stores/toast", () => ({
  toast: { show: vi.fn(), clear: vi.fn() },
}));

vi.mock("$lib/stores/config", () => ({
  setContactMuted: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn().mockResolvedValue(undefined),
}));

import { rename, archive } from "$lib/stores/contacts";
import { toast } from "$lib/stores/toast";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

function fakeSummary(overrides: Partial<{ nickname: string | null }> = {}): import("$lib/ipc/types").ContactSummary {
  return {
    pubkey: FAKE_PK,
    nickname: "Bob",
    onion: FAKE_ONION,
    card_version: 1n,
    added_at: 0n,
    unread_count: 0n,
    last_message_preview: null,
    last_ts_recv: null,
    group_state: "active",
    last_read_row_id: null,
    muted: false,
    peer_mailboxes: [],
    welcome_failed: false,
    ...overrides,
  };
}

describe("ContactDetailsPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders pubkey short hash in 4…4 format", () => {
    const { getByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    expect(getByText(/7aa2c4d1…00b3e9f7/)).toBeTruthy();
  });

  it("clicking pubkey copies the full hex and shows toast", async () => {
    const { getByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText(/7aa2c4d1…/));
    expect(writeText).toHaveBeenCalledWith(FAKE_PK);
    expect(toast.show).toHaveBeenCalledWith("Copied");
  });

  it("rename submit calls store.rename", async () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "Bobby" } });
    await fireEvent.click(getByText("Save"));
    expect(rename).toHaveBeenCalledWith(FAKE_PK, "Bobby");
  });

  it("rename submit disabled for empty input", () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary({ nickname: null }) },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    expect(input.value).toBe("");
    const save = getByText("Save") as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it("rename submit disabled for >64 chars", async () => {
    const { getByText, getByLabelText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    const input = getByLabelText("Nickname") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "x".repeat(65) } });
    const save = getByText("Save") as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it("archive button opens ConfirmDialog with locked copy", async () => {
    const { getByText, findByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText("Archive"));
    expect(await findByText("Archive Bob?")).toBeTruthy();
    expect(
      await findByText(/Bob disappears from your contacts/i),
    ).toBeTruthy();
  });

  it("archive confirm calls store.archive", async () => {
    const { getByText, findByText } = render(ContactDetailsPanel, {
      props: { summary: fakeSummary() },
    });
    await fireEvent.click(getByText("Archive"));
    const confirmBtn = await findByText("Archive");
    await fireEvent.click(confirmBtn);
    expect(archive).toHaveBeenCalledWith(FAKE_PK);
  });
});
