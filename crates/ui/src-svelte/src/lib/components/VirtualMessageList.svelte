<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<!--
  VirtualMessageList: virtualised list of MessageRecord items.
  Phase 2.D additions:
   - top-of-list IntersectionObserver → conversation.loadOlder()
   - bottom-of-list IntersectionObserver → conversation.markReadIfAtBottom()
   - inline UnreadSeparator at row_id == unreadAnchorRowId
   - inline SkeletonBubble × 5 at the top during loadingOlder
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import type { MessageRecord } from "$lib/ipc/types";
  import {
    conversation,
    loadOlder,
    markReadIfAtBottom,
  } from "$lib/stores/conversation";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import MessageBubble from "./MessageBubble.svelte";
  import UnreadSeparator from "./UnreadSeparator.svelte";
  import SkeletonBubble from "./SkeletonBubble.svelte";
  import { focusedRowId, setFocusedRowId } from "$lib/stores/searchPalette";

  let { items }: { items: (MessageRecord | OptimisticMessage)[] } = $props();
  let scrollEl = $state<HTMLDivElement | undefined>(undefined);
  let topSentinel = $state<HTMLDivElement | undefined>(undefined);
  let bottomSentinel = $state<HTMLDivElement | undefined>(undefined);

  // Seed for the FIRST paint only: rows are measured after mount (#210), so a
  // wrong estimate costs one reflow rather than permanently mispositioning
  // every row below a tall one.
  const ESTIMATED_ROW_HEIGHT = 72;
  const SKELETON_COUNT = 5;

  type Row =
    | { kind: "skeleton"; key: string }
    | { kind: "separator"; key: string }
    | { kind: "message"; key: string; record: MessageRecord | OptimisticMessage };

  function rowKey(m: MessageRecord | OptimisticMessage): string {
    const tempId = (m as OptimisticMessage).__tempId;
    if (tempId) return `t-${tempId}`;
    return `r-${m.row_id}`;
  }

  let rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if ($conversation.loadingOlder) {
      for (let i = 0; i < SKELETON_COUNT; i++) {
        out.push({ kind: "skeleton", key: `skel-${i}` });
      }
    }
    const anchor = $conversation.unreadAnchorRowId;
    let separatorEmitted = false;
    for (const m of items) {
      out.push({ kind: "message", key: rowKey(m), record: m });
      if (anchor !== null && !separatorEmitted && m.row_id === anchor) {
        out.push({ kind: "separator", key: "unread-separator" });
        separatorEmitted = true;
      }
    }
    return out;
  });

  /**
   * Built ONCE, as soon as `scrollEl` is bound (#214).
   *
   * `count` is read untracked on purpose. If it were a dependency, every
   * appended message would re-run this and construct a NEW virtualizer — and a
   * new virtualizer starts with an EMPTY measurement cache, so every row
   * reverts to `ESTIMATED_ROW_HEIGHT`. Nothing repairs that: `measureRow`'s
   * update path only fires when a row's index changes, and appending at the end
   * shifts no existing index. The rows below a tall one are then positioned
   * from a 72px estimate, so they overlap it, and a freshly appended row can be
   * placed outside the viewport — which reads as "the message never arrived".
   * Remounting rebuilt the cache, which is why closing and reopening the
   * conversation appeared to fix it.
   *
   * The row count is kept current by the `setOptions` effect below.
   */
  let virtualizer = $derived.by(() => {
    if (!scrollEl) return null;
    // Rebuild when the CONVERSATION changes identity — but never on append.
    // The measurement cache is keyed by row index, so carrying it into another
    // conversation makes its rows inherit the previous one's heights. Visible
    // rows remount and re-measure, but rows outside the rendered window keep
    // the stale heights, so getTotalSize() — and the scroll extent — stays
    // wrong until you scroll far enough to render them.
    void $conversation.contact;
    const initialCount = untrack(() => rows.length);
    return createVirtualizer<HTMLDivElement, HTMLDivElement>({
      count: initialCount,
      getScrollElement: () => scrollEl!,
      estimateSize: () => ESTIMATED_ROW_HEIGHT,
      overscan: 5,
    });
  });

  // Push row-count changes into the LIVE virtualizer (#214). The svelte adapter
  // merges `setOptions` over the current options, so the measurement cache —
  // the state that keeps tall rows positioned correctly — survives an append.
  // `appliedCount` is a plain `let`: it is bookkeeping, not reactive state, and
  // it keeps the effect from re-applying a count that is already in force.
  let appliedCount = -1;
  // Also tracked so a REBUILT virtualizer gets the count pushed into it even
  // when the row total happens to be unchanged across the switch.
  let appliedFor: unknown = null;
  // `$effect.pre`, not `$effect`: this must land BEFORE the DOM update.
  // `virtualItems`/`totalHeight` are pull-based deriveds read during render,
  // so a plain `$effect` (which flushes after) would lay the list out while the
  // virtualizer still had the OLD count — the appended row would fall outside
  // the virtual range and out of `getTotalSize()`, and nothing would recompute
  // afterwards because the adapter republishes the same instance identity.
  $effect.pre(() => {
    const count = rows.length;
    if (!virtualizer) return;
    if (virtualizer === appliedFor && count === appliedCount) return;
    appliedFor = virtualizer;
    appliedCount = count;
    untrack(() => {
      $virtualizer?.setOptions({ count });
    });
  });

  /**
   * Hand a row element to the virtualizer so its real height replaces the
   * estimate (#210).
   *
   * `measureElement` reads `data-index` off the node to know which item it is,
   * and observes the element, so a row that changes height AFTER first layout —
   * an inline image finishing decode is the case that exposed this — is
   * re-measured rather than leaving the rows below it overlapping.
   */
  function measureRow(node: HTMLDivElement, index: number) {
    const apply = (i: number) => {
      // virtual-core resolves which item a measurement belongs to by reading
      // `data-index` off the node AT CALL TIME, so set it before measuring
      // rather than relying on attribute-vs-action flush ordering.
      node.dataset.index = String(i);
      $virtualizer?.measureElement(node);
    };
    apply(index);
    return {
      // Takes the index as its argument specifically so this runs: an action
      // with no argument never has `update` called. `loadOlder` prepends rows,
      // and the keyed each-block reuses these DOM nodes while their indices
      // shift, so without re-measuring, cached heights stay bound to the
      // indices the nodes used to have — offsets drift, rows overlap again,
      // and the scroll position jumps.
      update(next: number) {
        apply(next);
      },
    };
  }

  // Track which row is currently highlighted for the focus-jump animation.
  let highlightRowId = $state<bigint | null>(null);

  /**
   * `rows.length` is a dependency here on purpose (#214).
   *
   * The virtualizer is now long-lived, and the adapter republishes the SAME
   * instance object (`derived(writable, (i) => Object.assign(i, {...}))`), so
   * the value behind `$virtualizer` does not change identity when the row count
   * changes. Reading `rows.length` gives these deriveds a dependency that DOES
   * change on an append, so a newly arrived message is laid out immediately
   * instead of only after the conversation is closed and reopened.
   */
  let virtualItems = $derived.by(() => {
    rows.length;
    return $virtualizer?.getVirtualItems() ?? [];
  });
  let totalHeight = $derived.by(() => {
    rows.length;
    return $virtualizer?.getTotalSize() ?? 0;
  });

  // Autoscroll ("tail") — follow the latest message, but only while the user is
  // already at (or near) the bottom. If they've scrolled up to read history,
  // don't yank them back down.
  let stickToBottom = $state(true);
  // True while an auto-scroll is in flight, so `onListScroll` can tell our own
  // scrolling apart from the user's (#222).
  let programmaticScroll = false;

  // Reset tail-scroll state when the active contact changes so that switching
  // conversations always starts pinned to the bottom, regardless of how far the
  // user had scrolled in the previous thread.
  $effect(() => {
    void $conversation.contact; // track the active contact
    stickToBottom = true;
  });

  function onListScroll() {
    if (!scrollEl) return;
    const dist = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight;
    // Ignore scroll events we caused ourselves (#222): the auto-scroll above
    // fires again as rows are measured, and a programmatic scroll that lands
    // short would otherwise compute dist >= 80 and clear the flag — leaving the
    // list no longer following new messages through no user action.
    if (programmaticScroll) return;
    stickToBottom = dist < 80;
  }

  // When a message is appended and we're stuck to the bottom, scroll to the end
  // so the latest is visible.
  // Which row we last auto-scrolled for, and whether we are still waiting for
  // its measured height to land (#222).
  let scrolledForKey: string | null = null;
  let awaitingMeasure = false;

  $effect(() => {
    const last = rows.length ? rows[rows.length - 1].key : null;
    // Depend on the measured total so the scroll re-runs once a newly appended
    // row's real height replaces ESTIMATED_ROW_HEIGHT (#222) — otherwise the
    // message lands below the fold.
    const measured = totalHeight;
    void measured;
    if (!scrollEl || !stickToBottom || last === null) return;

    if (last !== scrolledForKey) {
      // A row was appended at the END. Scroll now (on the estimate) and once
      // more when its measurement lands.
      scrolledForKey = last;
      awaitingMeasure = true;
    } else if (awaitingMeasure) {
      // That measurement has now landed.
      awaitingMeasure = false;
    } else {
      // The total changed but the last row did not: loadOlder PREPENDED rows.
      // Scrolling here would drag the reader away from the history they just
      // asked for, and races the top-sentinel into loading another page.
      return;
    }

    const el = scrollEl;
    requestAnimationFrame(() => {
      // Re-check: the user may have scrolled up between scheduling this and it
      // running. Measurement schedules extra scrolls, so without this a reader
      // browsing history gets pulled back to the latest message.
      if (!stickToBottom) return;
      programmaticScroll = true;
      el.scrollTop = el.scrollHeight;
      // Cleared on the next frame: the scroll event is dispatched
      // asynchronously, after this assignment returns.
      requestAnimationFrame(() => {
        programmaticScroll = false;
      });
    });
  });

  $effect(() => {
    if (!topSentinel) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          void loadOlder();
        }
      },
      { root: scrollEl, rootMargin: "100px 0px 0px 0px" },
    );
    obs.observe(topSentinel);
    return () => obs.disconnect();
  });

  $effect(() => {
    if (!bottomSentinel || !scrollEl) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        for (let i = items.length - 1; i >= 0; i--) {
          const r = items[i];
          if (typeof r.row_id === "bigint" && r.row_id > 0n) {
            // mark-read is gated exclusively on this bottom-sentinel intersection;
            // a search-jump scrolls to the focused row (never the bottom), so
            // this observer does NOT fire on a deep-link, preserving 2.D semantics.
            markReadIfAtBottom(r.row_id);
            return;
          }
        }
      },
      { root: scrollEl, threshold: 0.5 },
    );
    obs.observe(bottomSentinel);
    return () => obs.disconnect();
  });

  // Scroll-to-row effect for search-palette deep links.
  // Watches focusedRowId (set by SearchPalette.pick()); scrolls the matching
  // DOM element into view (block: center) and briefly highlights it (1200 ms).
  // Does NOT advance the read cursor — mark-read remains gated on the bottom
  // sentinel above.
  // Limitation (MVP): if the focused row isn't in the loaded set yet, this
  // silently does nothing; a paged "jump-to-row" loader is tracked as follow-up.
  $effect(() => {
    const id = $focusedRowId;
    if (id === null) return;
    // Clear the store immediately so re-firing only happens on a fresh pick.
    setFocusedRowId(null);
    // Use rAF so the virtualizer has had a chance to render.
    requestAnimationFrame(() => {
      const el = document.querySelector(`[data-row-id="${id}"]`) as HTMLElement | null;
      if (el) {
        el.scrollIntoView({ block: "center", behavior: "smooth" });
        highlightRowId = id;
        setTimeout(() => {
          highlightRowId = null;
        }, 1200);
      } else {
        console.warn("focus_row_id", id, "not in loaded set — scroll skipped (MVP)");
      }
    });
  });
</script>

<div class="list" bind:this={scrollEl} data-message-count={items.length} onscroll={onListScroll}>
  <div bind:this={topSentinel} class="sentinel"></div>
  <div style="height: {totalHeight}px; position: relative;">
    {#each virtualItems as row (rows[row.index]?.key ?? row.index)}
      <!-- data-index is set by measureRow, which writes it immediately before
           measuring; a declarative attribute here would be a second writer of
           the same value. -->
      <div
        use:measureRow={row.index}
        style="position: absolute; top: 0; left: 0; width: 100%; transform: translateY({row.start}px);"
      >
        {#if rows[row.index]}
          {#if rows[row.index].kind === "message"}
            <MessageBubble
              record={(rows[row.index] as { kind: "message"; key: string; record: MessageRecord | OptimisticMessage }).record}
              highlighted={highlightRowId !== null && (rows[row.index] as { kind: "message"; key: string; record: MessageRecord | OptimisticMessage }).record.row_id === highlightRowId}
            />
          {:else if rows[row.index].kind === "separator"}
            <UnreadSeparator />
          {:else if rows[row.index].kind === "skeleton"}
            <SkeletonBubble />
          {/if}
        {/if}
      </div>
    {/each}
  </div>
  <div bind:this={bottomSentinel} class="sentinel"></div>
</div>

<style>
  .list { height: 100%; overflow-y: auto; padding: var(--s-3); box-sizing: border-box; }
  .sentinel { height: 1px; }
</style>
