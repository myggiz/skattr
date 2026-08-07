// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("$lib/ipc/tauri", () => ({
  ipcClient: {
    request: vi.fn().mockResolvedValue({ resp: "ok", data: null }),
  },
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { ipcClient } from "$lib/ipc/tauri";
import { invoke } from "@tauri-apps/api/core";
import { wipeAllData, exportBackup, patchConfig } from "./config";
import type { ConfigPatch } from "$lib/ipc/types";

const nullPatch: ConfigPatch = {
  history_retention_days: null,
  direct_timeout_secs: null,
  notification_mode: null,
  close_to_tray: null,
  start_minimised: null,
  persist_logs_to_disk: null,
  download_dir: null,
};

describe("patchConfig — close_to_tray live sync (issue #31)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    (ipcClient.request as ReturnType<typeof vi.fn>).mockResolvedValue({
      resp: "ok",
      data: null,
    });
  });
  afterEach(() => vi.useRealTimers());

  it("updates the live tray sentinel after a successful flush that sets close_to_tray", async () => {
    const p = patchConfig({ ...nullPatch, close_to_tray: true });
    await vi.advanceTimersByTimeAsync(500);
    await p;
    expect(invoke).toHaveBeenCalledWith("set_close_to_tray", { enabled: true });
  });

  it("does not touch the tray sentinel when the patch omits close_to_tray", async () => {
    const p = patchConfig({ ...nullPatch, start_minimised: true });
    await vi.advanceTimersByTimeAsync(500);
    await p;
    expect(invoke).not.toHaveBeenCalledWith("set_close_to_tray", expect.anything());
  });
});

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
