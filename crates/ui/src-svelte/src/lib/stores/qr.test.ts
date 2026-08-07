// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { renderInviteQr, _resetCacheForTest } from "./qr";

describe("qr store", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    _resetCacheForTest();
  });

  it("calls render_invite_qr Tauri command on first render", async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue("<svg>x</svg>");
    const svg = await renderInviteQr("skattr://invite/v1#aaa");
    expect(svg).toBe("<svg>x</svg>");
    expect(invoke).toHaveBeenCalledWith("render_invite_qr", {
      url: "skattr://invite/v1#aaa",
    });
  });

  it("caches subsequent calls for the same URL", async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue("<svg>cached</svg>");
    const a = await renderInviteQr("skattr://invite/v1#bbb");
    const b = await renderInviteQr("skattr://invite/v1#bbb");
    expect(a).toBe(b);
    expect(invoke).toHaveBeenCalledOnce();
  });
});
