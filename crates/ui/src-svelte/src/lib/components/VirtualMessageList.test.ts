// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// #210: rows were positioned absolutely from a constant 72px estimate and never
// measured, so any taller row (a multi-line message, and dramatically an
// inline image preview) was overlapped by the row after it.
//
// jsdom has no layout — every element measures 0 — so this cannot assert real
// geometry. What it CAN assert is the wiring that makes measurement possible:
// each row must carry `data-index` (virtual-core reads it to know which item a
// measured element belongs to) and must be handed to `measureElement`. Those
// two were exactly what was missing.

import { describe, expect, test, vi } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";
import { readable } from "svelte/store";

const { measureSpy } = vi.hoisted(() => ({ measureSpy: vi.fn() }));

// jsdom has neither observer. The component uses IntersectionObserver for
// load-older / mark-read sentinels, which are not under test here.
class NoopObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  takeRecords(): [] {
    return [];
  }
}
vi.stubGlobal("IntersectionObserver", NoopObserver);
vi.stubGlobal("ResizeObserver", NoopObserver);

vi.mock("@tanstack/svelte-virtual", () => ({
  createVirtualizer: () =>
    readable({
      getVirtualItems: () => [
        { index: 0, start: 0, size: 72, key: 0 },
        { index: 1, start: 72, size: 72, key: 1 },
      ],
      getTotalSize: () => 144,
      measureElement: measureSpy,
    }),
}));

import VirtualMessageList from "./VirtualMessageList.svelte";
import type { MessageRecord } from "$lib/ipc/types";

function record(rowId: number, body: string): MessageRecord {
  return {
    row_id: BigInt(rowId),
    message_id: rowId.toString(16).padStart(32, "0"),
    contact: "ab".repeat(32),
    direction: "incoming",
    kind: { kind: "text", body },
    mls_generation: 1n,
    ts_daemon_recv: 1_700_000_000n,
    ts_envelope: 1_700_000_000n,
    delivered_at: null,
  };
}

describe("VirtualMessageList row measurement (#210)", () => {
  test("each rendered row carries data-index and is measured", async () => {
    measureSpy.mockClear();
    const { container } = render(VirtualMessageList, {
      props: { items: [record(1, "first"), record(2, "second")] },
    });

    // The virtualizer is only created once `scrollEl` is bound, so rows appear
    // after the first effect flush.
    await tick();
    await tick();

    const rows = container.querySelectorAll("[data-index]");
    expect(rows.length, "rows must carry data-index for virtual-core to key measurements").toBe(2);
    expect(Array.from(rows).map((r) => r.getAttribute("data-index"))).toEqual(["0", "1"]);

    // Without this the virtualizer keeps its constant estimate forever and
    // tall rows collide.
    expect(measureSpy, "each row element must be handed to measureElement").toHaveBeenCalledTimes(2);
  });
});
