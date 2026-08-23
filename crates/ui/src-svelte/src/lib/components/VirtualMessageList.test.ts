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
import { readable, writable } from "svelte/store";

const { measureSpy, createSpy, setOptionsSpy, virtualItems } = vi.hoisted(() => ({
  measureSpy: vi.fn(),
  // #214: a rebuild of the virtualizer drops every measured row height, so the
  // regression guard counts constructions and setOptions calls.
  createSpy: vi.fn(),
  setOptionsSpy: vi.fn(),
  // Mutable so a test can simulate loadOlder prepending rows, which shifts the
  // index of every already-rendered row.
  virtualItems: {
    current: [
      { index: 0, start: 0, size: 72, key: 0 },
      { index: 1, start: 72, size: 72, key: 1 },
    ],
  },
}));

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
  createVirtualizer: (options: { count: number }) => {
    createSpy(options);
    const instance = {
      getVirtualItems: () => virtualItems.current,
      getTotalSize: () => 144,
      measureElement: measureSpy,
      setOptions: (next: { count: number }) => {
        setOptionsSpy(next);
        // The real @tanstack/svelte-virtual adapter republishes the instance
        // from setOptions (via its onChange wrapper). Consumers re-read
        // getVirtualItems() off that emission, so the mock must do it too.
        store.set(instance);
      },
    };
    const store = writable(instance);
    return store;
  },
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

  // #211 review: exercise measureRow.update, NOT a remount.
  //
  // The each-block key is `rows[row.index]?.key ?? row.index`. If the virtual
  // indices move beyond the loaded rows, that key falls back to the raw index,
  // the keys change, Svelte destroys and recreates the nodes, and the ACTION
  // MOUNTS AGAIN — which passes even when update-path remeasurement is broken.
  //
  // To reach `update`, the same records must still sit at the new indices, so
  // the keys are unchanged and the DOM nodes are preserved. That is what
  // loadOlder actually does: it prepends history above rows already on screen.
  test("a preserved row whose index shifts is re-measured under its new index", async () => {
    const first = record(1, "first");
    const second = record(2, "second");
    const { container, rerender } = render(VirtualMessageList, {
      props: { items: [first, second] },
    });
    await tick();
    await tick();
    expect(
      Array.from(container.querySelectorAll("[data-index]")).map((r) => r.getAttribute("data-index")),
    ).toEqual(["0", "1"]);
    const nodesBefore = Array.from(container.querySelectorAll("[data-index]"));
    measureSpy.mockClear();

    // Ten older messages arrive above: `first`/`second` are now at 10/11, so
    // `rows[10]`/`rows[11]` resolve to the same records and the keys — and
    // therefore the DOM nodes — are unchanged.
    const older = Array.from({ length: 10 }, (_, i) => record(100 + i, `older-${i}`));
    virtualItems.current = [
      { index: 10, start: 0, size: 72, key: 10 },
      { index: 11, start: 72, size: 72, key: 11 },
    ];
    await rerender({ items: [...older, first, second] });
    await tick();
    await tick();

    const nodesAfter = Array.from(container.querySelectorAll("[data-index]"));
    expect(nodesAfter[0], "the row element must be reused, not remounted").toBe(nodesBefore[0]);
    expect(nodesAfter.map((r) => r.getAttribute("data-index"))).toEqual(["10", "11"]);
    expect(
      measureSpy.mock.calls.length,
      "a preserved row must be re-measured when its index shifts",
    ).toBeGreaterThan(0);
  });
});

// #214: appending a message rebuilt the virtualizer, and a fresh virtualizer has
// an empty measurement cache — every row fell back to the 72px estimate. Nothing
// repaired it: `measureRow`'s update path only runs when a row's INDEX changes,
// and appending at the end shifts no existing index. Rows below a tall row were
// then positioned by bad arithmetic, so they overlapped it and a newly appended
// row could land off-screen ("the message never arrived"). Remounting the
// conversation rebuilt the cache, which is why close/reopen was the workaround.
describe("VirtualMessageList virtualizer lifetime (#214)", () => {
  test("appending a message does not reconstruct the virtualizer", async () => {
    virtualItems.current = [
      { index: 0, start: 0, size: 72, key: 0 },
      { index: 1, start: 72, size: 72, key: 1 },
    ];
    createSpy.mockClear();
    setOptionsSpy.mockClear();

    const first = record(1, "first");
    const second = record(2, "second");
    const { rerender } = render(VirtualMessageList, { props: { items: [first, second] } });
    await tick();
    await tick();

    expect(createSpy, "the virtualizer is constructed once, after scrollEl binds").toHaveBeenCalledTimes(1);

    // A third message arrives into the ALREADY-MOUNTED list. This is the field
    // case: an incoming message while the conversation is open.
    virtualItems.current = [
      { index: 0, start: 0, size: 72, key: 0 },
      { index: 1, start: 72, size: 72, key: 1 },
      { index: 2, start: 144, size: 72, key: 2 },
    ];
    await rerender({ items: [first, second, record(3, "third")] });
    await tick();
    await tick();

    expect(
      createSpy,
      "appending must NOT rebuild the virtualizer — a rebuild discards every measured row height",
    ).toHaveBeenCalledTimes(1);
    expect(
      setOptionsSpy.mock.calls.some((c) => c[0]?.count === 3),
      "the new row count must reach the existing virtualizer via setOptions",
    ).toBe(true);
  });
});
