// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach } from "vitest";
import { get } from "svelte/store";
import {
  attachments,
  markQueued,
  applyManifest,
  applyProgress,
  applyReceived,
  markAvailable,
  applyFailed,
  markRetrying,
  attachmentFor,
} from "./attachments";

describe("attachments store", () => {
  beforeEach(() => attachments.set(new Map()));

  test("markQueued seeds a queued entry", () => {
    markQueued("aa", { filename: "f.bin", size: 10, total: 3 });
    expect(attachmentFor("aa")).toEqual({
      status: "queued", received: 0, total: 3, filename: "f.bin", size: 10,
    });
  });

  test("applyManifest fills static fields without clobbering progress", () => {
    applyProgress("bb", 2, 5);
    applyManifest("bb", { filename: "p.jpg", mime: "image/jpeg", size: 99, total: 5 });
    const s = attachmentFor("bb")!;
    expect(s.received).toBe(2);
    expect(s.filename).toBe("p.jpg");
    expect(s.mime).toBe("image/jpeg");
    expect(s.status).toBe("receiving");
  });

  test("applyProgress sets receiving + counts", () => {
    applyProgress("cc", 1, 4);
    expect(attachmentFor("cc")).toMatchObject({ status: "receiving", received: 1, total: 4 });
  });

  test("applyReceived marks complete without a path", () => {
    applyReceived("aa".repeat(16), { filename: "f.bin", mime: "application/octet-stream", size: 10 });
    const s = attachmentFor("aa".repeat(16))!;
    expect(s.status).toBe("complete");
    expect("path" in s).toBe(false);
  });

  test("markAvailable flips a complete attachment available", () => {
    markAvailable("bb".repeat(16), { filename: "g.bin", mime: "text/plain", size: 3 });
    const s = attachmentFor("bb".repeat(16))!;
    expect(s.status).toBe("complete");
    expect(s.available).toBe(true);
  });

  test("applyFailed marks failed with reason", () => {
    applyProgress("ee", 1, 4);
    applyFailed("ee", "timeout");
    expect(attachmentFor("ee")).toMatchObject({ status: "failed", reason: "timeout" });
  });

  test("markRetrying clears the failure but keeps received chunks (#144)", () => {
    applyProgress("gg", 3, 8);
    applyFailed("gg", "request timeout");
    markRetrying("gg");
    const s = attachmentFor("gg")!;
    expect(s).toMatchObject({ status: "queued", retrying: true, received: 3, total: 8 });
    expect(s.reason).toBeUndefined();
  });

  test("a retry that then makes progress drops the retrying flag (#144)", () => {
    applyFailed("hh", "timeout");
    markRetrying("hh");
    applyProgress("hh", 1, 4);
    expect(attachmentFor("hh")).toMatchObject({ status: "receiving", retrying: false });
  });

  test("updates are immutable (new Map each time)", () => {
    markQueued("ff", { total: 1 });
    const first = get(attachments);
    applyProgress("ff", 1, 1);
    expect(get(attachments)).not.toBe(first);
  });
});
