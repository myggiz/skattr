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

  it("concurrent handleStreamClosed calls: second call is dropped while first is in flight", async () => {
    __resetForTest();

    // A controlled promise lets us hold the first resubscribe call in-flight
    // until we are ready to resolve it, making the race deterministic.
    let resolveFirst!: () => void;
    const firstCallInflight = new Promise<void>((r) => { resolveFirst = r; });

    const resubscribe = vi.fn()
      // First invocation: blocks until we manually resolve.
      .mockReturnValueOnce(firstCallInflight)
      // Subsequent invocations (must not happen): succeed immediately.
      .mockResolvedValue(undefined);

    // Start first handleStreamClosed — it awaits the slow resubscribe.
    const first = handleStreamClosed(resubscribe, { baseDelayMs: 0 });

    // Fire a second concurrent call immediately — must be dropped by the guard.
    const second = handleStreamClosed(resubscribe, { baseDelayMs: 0 });

    // Resolve the in-flight first call so both promises can settle.
    resolveFirst();
    await Promise.all([first, second]);

    // resubscribe must have been called exactly once (the second call was a no-op).
    expect(resubscribe).toHaveBeenCalledOnce();
    expect(get(connection).state).toBe("live");
  });
});
