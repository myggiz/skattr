<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
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

  let virtualItems = $derived($virtualizer?.getVirtualItems() ?? []);
  let totalHeight = $derived($virtualizer?.getTotalSize() ?? 0);

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
</script>

<div class="list" bind:this={scrollEl} data-message-count={items.length}>
  <div bind:this={topSentinel} class="sentinel"></div>
  <div style="height: {totalHeight}px; position: relative;">
    {#each virtualItems as row (rows[row.index]?.key ?? row.index)}
      <div
        style="position: absolute; top: 0; left: 0; width: 100%; transform: translateY({row.start}px);"
      >
        {#if rows[row.index]}
          {#if rows[row.index].kind === "message"}
            <MessageBubble record={(rows[row.index] as { kind: "message"; key: string; record: MessageRecord | OptimisticMessage }).record} />
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
