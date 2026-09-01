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
    grouped = false,
  }: {
    record: MessageRecord | OptimisticMessage;
    highlighted?: boolean;
    /** Continues the previous message's turn — stacked tight, no tail. */
    grouped?: boolean;
  } = $props();

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
  <div class="bubble" class:outgoing={isOutgoing} class:grouped class:focus-highlight={highlighted} data-row-id={record.row_id}>
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
    border: 1px solid var(--hairline);
    padding: var(--s-2) var(--s-3);
    /* The corner nearest the speaker is squared, so which side a message came
       from survives even when colour does not (light theme, dimmed window). */
    border-radius: 16px 16px 16px 4px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .bubble.outgoing {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg);
    border-radius: 16px 16px 4px 16px;
    margin-left: auto;
  }
  /* Only the first bubble of a turn carries the tail; the rest stack tight so
     a burst of messages reads as one utterance. Declared AFTER .outgoing, and
     with a matching-specificity companion, so it is not cancelled by it. */
  .bubble.grouped { border-radius: 16px; margin-top: 1px; }
  .bubble.outgoing.grouped { border-radius: 16px; }
  .body { margin: 0; white-space: pre-wrap; word-break: break-word; }
  .meta {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--s-1);
    margin-top: var(--s-1);
  }
  /* A timestamp is machine fact, so it takes the data face. */
  .ts { color: var(--text-muted); font: var(--t-label); letter-spacing: 0.04em; }
  /* #197: a hard-coded white at 70% was 1.69:1 on the accent fill. Inherit the
     bubble foreground so it tracks the theme. No opacity: this is TEXT, so it
     needs 4.5:1 (WCAG 1.4.3), and compositing even 0.75 toward the accent drops
     it to 3.29:1 in light mode. Full strength gives 4.56:1 / 7.61:1. */
  .bubble.outgoing .ts { color: currentColor; }
  /* A search hit is activity, not a control, so it takes the live accent. */
  .focus-highlight {
    outline: 2px solid var(--live);
    transition: outline 0.6s;
  }
</style>
