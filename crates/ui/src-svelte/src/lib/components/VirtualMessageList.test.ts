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

const { measureSpy, createSpy, setOptionsSpy, virtualItems, totalSize, emit } = vi.hoisted(() => ({
  totalSize: { current: 144 },
  // Set by the mock so a test can make the virtualizer republish itself with a
  // larger total — what really happens a frame after a tall row is measured.
  emit: { republish: () => {} },
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
      getTotalSize: () => totalSize.current,
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
    emit.republish = () => store.set(instance);
    return store;
  },
}));

import VirtualMessageList from "./VirtualMessageList.svelte";
import { conversation } from "$lib/stores/conversation";
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
    dismissed_at: null,
    failed_reason: null,
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

  // #220 review: the measurement cache is keyed by row index, so keeping one
  // virtualizer across a conversation SWITCH makes the new conversation's rows
  // inherit the previous one's heights. Visible rows remount and re-measure,
  // but rows outside the rendered window keep the stale heights, so
  // getTotalSize() — and the scroll extent — stays wrong until they render.
  //
  // Rebuilding on switch is safe; rebuilding on APPEND is what #214 fixed.
  test("switching conversations rebuilds the virtualizer, appending does not", async () => {
    const a = "aa".repeat(32);
    const b = "bb".repeat(32);
    conversation.set({
      contact: a,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });

    const first = record(1, "first");
    const { rerender } = render(VirtualMessageList, { props: { items: [first] } });
    await tick();
    await tick();
    createSpy.mockClear();

    // Same conversation, one more message: must reuse the virtualizer.
    await rerender({ items: [first, record(2, "second")] });
    await tick();
    await tick();
    expect(
      createSpy,
      "appending must not rebuild (it would discard every measured height)",
    ).toHaveBeenCalledTimes(0);

    // Different conversation: must NOT carry the index-keyed cache over.
    conversation.set({
      contact: b,
      messages: [],
      nextBeforeId: null,
      loadingOlder: false,
      unreadAnchorRowId: null,
      readCursor: 0n,
    });
    await rerender({ items: [record(10, "other-first"), record(11, "other-second")] });
    await tick();
    await tick();
    expect(
      createSpy,
      "switching conversations must rebuild so heights are not inherited",
    ).toHaveBeenCalledTimes(1);
  });

  // #222: a newly appended row is still at ESTIMATED_ROW_HEIGHT for the first
  // frame, so an auto-scroll keyed only on the row COUNT lands on a bottom
  // computed from the estimate; the row is then measured, the total grows, and
  // the message ends up below the fold.
  //
  // jsdom has no layout — scrollHeight is always 0 — so this cannot assert the
  // view lands at the bottom. It pins the dependency that makes the re-scroll
  // happen, and it must do so WITHOUT touching `items`: re-rendering with a new
  // items array re-runs the effect by itself, which would make this pass even
  // with the dependency removed (it did, first time round).
  test("a measured height change alone re-runs the auto-scroll", async () => {
    const rafSpy = vi.fn((cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
    vi.stubGlobal("requestAnimationFrame", rafSpy);

    render(VirtualMessageList, { props: { items: [record(1, "first"), record(2, "second")] } });
    await tick();
    await tick();
    rafSpy.mockClear();

    // No prop change at all: only the measured total grows, exactly as it does
    // one frame after a tall row mounts and is measured.
    totalSize.current = 520;
    emit.republish();
    await tick();
    await tick();

    expect(
      rafSpy.mock.calls.length,
      "growing the measured total must re-run the auto-scroll, or the new row stays below the fold",
    ).toBeGreaterThan(0);

    vi.unstubAllGlobals();
    vi.stubGlobal("IntersectionObserver", NoopObserver);
    vi.stubGlobal("ResizeObserver", NoopObserver);
  });

  // #223 review: the auto-scroll re-checks the flag INSIDE the frame callback.
  // A measurement schedules the callback; if the user scrolls up in that gap,
  // running it unconditionally drags them back to the latest message. #222 made
  // this more likely by scheduling an extra scroll per measurement.
  test("a scroll queued before the user scrolls up is abandoned", async () => {
    // Deferred rAF: hold the callback so the race can actually be staged.
    const queued: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    });

    const { container } = render(VirtualMessageList, {
      props: { items: [record(1, "first"), record(2, "second")] },
    });
    await tick();
    await tick();

    const list = container.querySelector(".list") as HTMLElement;
    // jsdom reports 0 for every dimension, so give the element a geometry in
    // which the user is clearly scrolled AWAY from the bottom.
    Object.defineProperty(list, "scrollHeight", { value: 1000, configurable: true });
    Object.defineProperty(list, "clientHeight", { value: 100, configurable: true });
    list.scrollTop = 0;

    // A measurement lands, queueing a scroll.
    totalSize.current = 900;
    emit.republish();
    await tick();
    await tick();
    expect(queued.length, "a measurement should queue a scroll").toBeGreaterThan(0);

    // The user scrolls up before that frame runs: dist = 1000 - 0 - 100 = 900.
    list.dispatchEvent(new Event("scroll"));
    await tick();

    const before = list.scrollTop;
    queued.forEach((cb) => cb(0));
    expect(
      list.scrollTop,
      "the queued scroll must be abandoned once the user has scrolled away",
    ).toBe(before);

    vi.unstubAllGlobals();
    vi.stubGlobal("IntersectionObserver", NoopObserver);
    vi.stubGlobal("ResizeObserver", NoopObserver);
  });
});
