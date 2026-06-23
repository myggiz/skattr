// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test, vi, beforeEach } from "vitest";

// Mock the Tauri core module before importing the SUT.
vi.mock("@tauri-apps/api/core");

import { invoke } from "@tauri-apps/api/core";
import {
  formatBytes,
  isImage,
  mimeIconName,
  decodeManifest,
  decodeManifestMemo,
  MANIFEST_SIZE_HARD,
  MANIFEST_SIZE_SOFT,
} from "./attachments";

const invokeMock = vi.mocked(invoke);

describe("formatBytes", () => {
  test("scales units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1_572_864)).toBe("1.5 MiB");
  });
});

describe("mime helpers", () => {
  test("isImage", () => {
    expect(isImage("image/png")).toBe(true);
    expect(isImage("application/pdf")).toBe(false);
    expect(isImage(undefined)).toBe(false);
  });
  test("mimeIconName", () => {
    expect(mimeIconName("image/jpeg")).toBe("image");
    expect(mimeIconName("text/plain")).toBe("file");
    expect(mimeIconName(undefined)).toBe("file");
  });
});

describe("size constants", () => {
  test("match daemon caps", () => {
    expect(MANIFEST_SIZE_HARD).toBe(100 * 1024 * 1024);
    expect(MANIFEST_SIZE_SOFT).toBe(10 * 1024 * 1024);
  });
});

describe("decodeManifest", () => {
  beforeEach(() => invokeMock.mockReset());

  test("passes raw bytes to the shell command and returns the summary", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: "ab".repeat(16), filename: "p.jpg", mime: "image/jpeg", total_size: 5,
    });
    const out = await decodeManifest({ kind: "file", manifest: [1, 2, 3] as unknown as string });
    expect(invokeMock).toHaveBeenCalledWith("decode_attachment_manifest", { manifest: [1, 2, 3] });
    expect(out.filename).toBe("p.jpg");
  });

  test("memo decodes once per message id", async () => {
    invokeMock.mockResolvedValue({
      attachment_id: "cd".repeat(16), filename: "x", mime: "text/plain", total_size: 1,
    });
    const m = { kind: "file", manifest: [9] as unknown as string } as const;
    await decodeManifestMemo("msg1", m);
    await decodeManifestMemo("msg1", m);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});
