// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({ resp: "ok", data: null }),
  },
}));

import { ipcClient } from "$lib/ipc/tauri";
import { wipeAllData, exportBackup } from "./config";

describe("wipeAllData", () => {
  beforeEach(() => vi.clearAllMocks());

  it("resolves when the daemon replies ok", async () => {
    (ipcClient.request as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      resp: "ok",
      data: null,
    });
    await expect(wipeAllData()).resolves.toBeUndefined();
    expect(ipcClient.request).toHaveBeenCalledWith({ cmd: "wipe_all_data" });
  });

  it("throws on error response", async () => {
    (ipcClient.request as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      resp: "err",
      data: { err: "internal", data: "boom" },
    });
    await expect(wipeAllData()).rejects.toThrow();
  });
});

describe("exportBackup", () => {
  beforeEach(() => vi.clearAllMocks());

  it("resolves when the daemon replies ok", async () => {
    (ipcClient.request as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      resp: "ok",
      data: null,
    });
    await expect(exportBackup("/tmp/backup.age")).resolves.toBeUndefined();
    expect(ipcClient.request).toHaveBeenCalledWith({
      cmd: "export_backup",
      dest_path: "/tmp/backup.age",
    });
  });

  it("throws a mapped error message on error response", async () => {
    (ipcClient.request as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      resp: "err",
      data: { err: "internal", data: "disk full" },
    });
    await expect(exportBackup("/tmp/backup.age")).rejects.toThrow(
      /something went wrong/i,
    );
  });
});
