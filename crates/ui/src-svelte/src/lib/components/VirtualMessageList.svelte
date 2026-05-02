<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<!--
  VirtualMessageList: renders a virtualised list of MessageRecord items.

  Uses @tanstack/svelte-virtual (Svelte 5 compatible) instead of
  svelte-virtual-list (which is unmaintained and does not support Svelte 5).
  Substitution recorded per plan risk note (Task 27).
-->
<script lang="ts">
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import type { MessageRecord } from "$lib/ipc/types";
  import MessageBubble from "./MessageBubble.svelte";

  let { items }: { items: MessageRecord[] } = $props();

  let scrollEl = $state<HTMLDivElement | undefined>(undefined);

  // Estimated row height in px; the virtualizer self-corrects after measurement.
  const ESTIMATED_ROW_HEIGHT = 72;

  let virtualizer = $derived(
    scrollEl
      ? createVirtualizer<HTMLDivElement, HTMLDivElement>({
          count: items.length,
          getScrollElement: () => scrollEl!,
          estimateSize: () => ESTIMATED_ROW_HEIGHT,
          overscan: 5,
        })
      : null,
  );

  let virtualItems = $derived($virtualizer?.getVirtualItems() ?? []);
  let totalHeight = $derived($virtualizer?.getTotalSize() ?? 0);
</script>

<div class="list" bind:this={scrollEl}>
  <div style="height: {totalHeight}px; position: relative;">
    {#each virtualItems as row (row.index)}
      <div
        style="position: absolute; top: 0; left: 0; width: 100%; transform: translateY({row.start}px);"
      >
        <MessageBubble record={items[row.index]} />
      </div>
    {/each}
  </div>
</div>

<style>
  .list { height: 100%; overflow-y: auto; padding: var(--s-3); box-sizing: border-box; }
</style>
