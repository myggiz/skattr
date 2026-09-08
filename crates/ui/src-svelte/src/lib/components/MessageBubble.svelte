<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import DeliveryIcon from "./DeliveryIcon.svelte";
  import FileAttachmentBubble from "./FileAttachmentBubble.svelte";
  import {
    delivery,
    deliveryToIconStatus,
    deliveryStateFromRecord,
    failureReasonFromStatus,
    hex16ToString,
  } from "$lib/stores/delivery";
  import { send, dismiss } from "$lib/stores/conversation";

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

  // Precedence for the persisted (non-optimistic) fields is pinned in one
  // place — deliveryStateFromRecord — so it isn't repeated or re-decided
  // here: delivered wins, a dismissal comes before a possibly-stale
  // failed_reason, and only then does failed_reason apply.
  let recordState = $derived(deliveryStateFromRecord(record));
  let dismissed = $derived(recordState === "dismissed");

  let hex = $derived(hex16ToString(record.message_id));

  let iconStatus = $derived.by(() => {
    if (!isOutgoing) return null;
    if (failed) return "failed" as const;
    if (optimistic) return "pending" as const;
    if (recordState === "delivered") return "delivered" as const;
    if (recordState === "dismissed" || recordState === "failed") return "failed" as const;
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

  // The record's own failed_reason is the durable source (survives a
  // reload); a live Failed event lands in the delivery map first, so it is
  // the fallback for the same in-memory window where iconStatus already
  // reads "failed" off that map (see iconStatus above).
  let failureReason = $derived(
    !dismissed && iconStatus === "failed"
      ? (record.failed_reason ?? failureReasonFromStatus($delivery.get(hex)))
      : null,
  );

  function resend(): void {
    void send(record.contact, body);
  }

  function handleDismiss(): void {
    void dismiss(record.message_id);
  }
</script>

{#if record.kind.kind === "file"}
  <FileAttachmentBubble {record} />
{:else}
  <div class="bubble" class:outgoing={isOutgoing} class:grouped class:dismissed class:focus-highlight={highlighted} data-row-id={record.row_id}>
    <p class="body">{body}</p>
    <div class="meta">
      <time class="ts">{new Date(tsMs).toLocaleTimeString()}</time>
      {#if isOutgoing && iconStatus}
        <DeliveryIcon status={iconStatus} title={iconTitle} />
      {/if}
    </div>
    {#if failureReason}
      <p class="failure">{failureReason}</p>
      <div class="failure-actions">
        <button type="button" onclick={resend}>Resend</button>
        <button type="button" onclick={handleDismiss}>Dismiss</button>
      </div>
    {/if}
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
  /* Dismissed keeps the bubble in place but visibly de-emphasized — no new
     colour token, just recede via opacity. */
  .bubble.dismissed { opacity: 0.6; }
  .failure { margin: var(--s-1) 0 0; font: var(--t-ui); color: var(--danger); }
  .bubble.outgoing .failure { color: var(--danger-on-accent); }
  .failure-actions { display: flex; gap: var(--s-1); margin-top: var(--s-1); }
  .failure-actions button {
    padding: 2px var(--s-2);
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--hairline);
    border-radius: 6px;
    font: var(--t-ui);
    cursor: pointer;
  }
</style>
