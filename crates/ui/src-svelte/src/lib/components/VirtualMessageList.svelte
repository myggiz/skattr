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

  let virtualizer = $derived(
    scrollEl
      ? createVirtualizer<HTMLDivElement, HTMLDivElement>({
          count: rows.length,
          getScrollElement: () => scrollEl!,
          estimateSize: () => ESTIMATED_ROW_HEIGHT,
          overscan: 5,
        })
      : null,
  );

  // Track which row is currently highlighted for the focus-jump animation.
  let highlightRowId = $state<bigint | null>(null);

  let virtualItems = $derived($virtualizer?.getVirtualItems() ?? []);
  let totalHeight = $derived($virtualizer?.getTotalSize() ?? 0);

  // Autoscroll ("tail") — follow the latest message, but only while the user is
  // already at (or near) the bottom. If they've scrolled up to read history,
  // don't yank them back down.
  let stickToBottom = $state(true);

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
    stickToBottom = dist < 80;
  }

  // When the message set grows (new message, or first load of a conversation)
  // and we're stuck to the bottom, scroll to the end so the latest is visible.
  $effect(() => {
    const n = rows.length; // track growth
    if (n === 0 || !stickToBottom || !scrollEl) return;
    const el = scrollEl;
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
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
      <div
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
