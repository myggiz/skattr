// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import InviteGenerateDialog from "./InviteGenerateDialog.svelte";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({
      resp: "ok",
      data: {
        result: "invite_created",
        data: {
          url: "skattr://invite/v1#test",
          key_package_id: "00".repeat(32),
          expires_at: 1_700_010_000,
        },
      },
    }),
  },
}));

vi.mock("$lib/stores/qr", () => ({
  renderInviteQr: vi.fn().mockResolvedValue("<svg>QR</svg>"),
}));

import { ipcClient } from "$lib/ipc/tauri";
import { renderInviteQr } from "$lib/stores/qr";

describe("InviteGenerateDialog", () => {
  it("default TTL is 24h (86400 s)", async () => {
    const onClose = vi.fn();
    const { getByText } = render(InviteGenerateDialog, { props: { onClose } });
    await fireEvent.click(getByText("Generate"));
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "create_invite",
      nickname: null,
      ttl_secs: 86400,
    });
  });

  it("renders QR after successful generate", async () => {
    const { getByText, findByText } = render(InviteGenerateDialog, {
      props: { onClose: vi.fn() },
    });
    await fireEvent.click(getByText("Generate"));
    await findByText(/skattr:\/\/invite/);
    expect(renderInviteQr).toHaveBeenCalledWith("skattr://invite/v1#test");
  });
});
