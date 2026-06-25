// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, it, expect, vi } from "vitest";
import { get } from "svelte/store";
import { connection, handleStreamClosed, __resetForTest } from "./connection";

describe("connection store", () => {
  it("starts live", () => {
    __resetForTest();
    expect(get(connection).state).toBe("live");
  });

  it("goes reconnecting then live on a successful re-subscribe", async () => {
    __resetForTest();
    const resubscribe = vi.fn().mockResolvedValue(undefined);
    await handleStreamClosed(resubscribe);
    expect(resubscribe).toHaveBeenCalledOnce();
    expect(get(connection).state).toBe("live");
  });

  it("goes dead after exhausting retries", async () => {
    __resetForTest();
    const resubscribe = vi.fn().mockRejectedValue(new Error("down"));
    await handleStreamClosed(resubscribe, { maxAttempts: 3, baseDelayMs: 0 });
    expect(resubscribe).toHaveBeenCalledTimes(3);
    expect(get(connection).state).toBe("dead");
  });
});
