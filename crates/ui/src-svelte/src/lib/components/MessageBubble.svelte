<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import DeliveryIcon from "./DeliveryIcon.svelte";
  import FileAttachmentBubble from "./FileAttachmentBubble.svelte";
  import { delivery, deliveryToIconStatus, hex16ToString } from "$lib/stores/delivery";

  let {
    record,
    highlighted = false,
  }: { record: MessageRecord | OptimisticMessage; highlighted?: boolean } = $props();

  let body = $derived(
    record.kind && record.kind.kind === "text" ? record.kind.body : "",
  );
  let isOutgoing = $derived(record.direction === "outgoing");
  let tsMs = $derived(Number(record.ts_daemon_recv) * 1000);

  let optimistic = $derived((record as OptimisticMessage).__optimistic === true);
  let failed = $derived((record as OptimisticMessage).__failed);

  let iconStatus = $derived.by(() => {
    if (!isOutgoing) return null;
    if (failed) return "failed" as const;
    if (optimistic) return "pending" as const;
    const hex = hex16ToString(record.message_id);
    return deliveryToIconStatus($delivery.get(hex));
  });

  let iconTitle = $derived.by(() => {
    if (failed) return failed;
    if (optimistic) return "Pending";
    return iconStatus === "delivered" ? "Delivered"
         : iconStatus === "sent"      ? "Delivered to mailbox"
         : iconStatus === "failed"    ? "Failed"
                                      : "Pending";
  });
</script>

{#if record.kind.kind === "file"}
  <FileAttachmentBubble {record} />
{:else}
  <div class="bubble" class:outgoing={isOutgoing} class:focus-highlight={highlighted} data-row-id={record.row_id}>
    <p class="body">{body}</p>
    <div class="meta">
      <time class="ts">{new Date(tsMs).toLocaleTimeString()}</time>
      {#if isOutgoing && iconStatus}
        <DeliveryIcon status={iconStatus} title={iconTitle} />
      {/if}
    </div>
  </div>
{/if}

<style>
  .bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .body { margin: 0; white-space: pre-wrap; word-break: break-word; }
  .meta {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--s-1);
    margin-top: var(--s-1);
  }
  .ts { color: var(--text-muted); font: var(--t-ui); }
  .bubble.outgoing .ts { color: rgba(255, 255, 255, 0.7); }
  .focus-highlight {
    outline: 2px solid var(--accent, #6c6);
    transition: outline 0.6s;
  }
</style>
