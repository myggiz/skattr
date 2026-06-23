<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { invoke, convertFileSrc } from "@tauri-apps/api/core";
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import { attachments, applyManifest } from "$lib/stores/attachments";
  import { delivery, deliveryToIconStatus, hex16ToString } from "$lib/stores/delivery";
  import { decodeManifestMemo, isImage, mimeIconName, formatBytes } from "$lib/attachments";
  import type { ManifestSummary } from "$lib/attachments";
  import { icons } from "$lib/icons";
  import { toast } from "$lib/stores/toast";
  import DeliveryIcon from "./DeliveryIcon.svelte";

  let { record }: { record: MessageRecord | OptimisticMessage } = $props();

  let isOutgoing = $derived(record.direction === "outgoing");
  // The optimistic outgoing path carries display fields directly (Task 10).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let optimisticName = $derived((record as any).__attachName as string | undefined);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let optimisticSize = $derived((record as any).__attachSize as number | undefined);

  let summary = $state<ManifestSummary | null>(null);
  let decodeFailed = $state(false);
  let imgBroken = $state(false);

  // Decode the manifest once per message id; on success seed the store's
  // static fields so the bubble can render filename/size even if no transfer
  // events have arrived yet.
  $effect(() => {
    if (record.kind.kind !== "file") return;
    const fileKind = record.kind;
    const mid = hex16ToString(record.message_id);
    decodeManifestMemo(mid, fileKind)
      .then((s) => {
        summary = s;
        applyManifest(s.attachment_id, {
          filename: s.filename, mime: s.mime, size: s.total_size, total: 0,
        });
      })
      .catch(() => (decodeFailed = true));
  });

  let aidHex = $derived(summary ? summary.attachment_id : null);
  let xferState = $derived(aidHex ? $attachments.get(aidHex) : undefined);

  // Display fields: prefer decoded manifest, fall back to optimistic send info.
  let filename = $derived(summary?.filename ?? optimisticName ?? "");
  let mime = $derived(summary?.mime);
  let size = $derived(summary?.total_size ?? optimisticSize);

  let receiving = $derived(!isOutgoing && xferState?.status === "receiving");
  let complete = $derived(!isOutgoing && xferState?.status === "complete" && !!xferState?.path);
  let failed = $derived(!isOutgoing && xferState?.status === "failed");
  let showImage = $derived(complete && isImage(mime) && !imgBroken);

  let pct = $derived(
    xferState && xferState.total > 0 ? Math.round((xferState.received / xferState.total) * 100) : 0,
  );
  let indeterminate = $derived(receiving && (!xferState || xferState.total === 0));

  let deliveryStatus = $derived(
    isOutgoing ? deliveryToIconStatus($delivery.get(hex16ToString(record.message_id))) : null,
  );

  async function doOpen() {
    if (!xferState?.path) return;
    try {
      await invoke("open_file", { path: xferState.path });
    } catch {
      toast.show("File not found");
    }
  }
  async function doReveal() {
    if (!xferState?.path) return;
    try {
      await invoke("reveal_in_folder", { path: xferState.path });
    } catch {
      toast.show("File not found");
    }
  }

  let iconGlyph = $derived(icons[mimeIconName(mime)]);
</script>

<div class="file-bubble" class:outgoing={isOutgoing} data-row-id={record.row_id}>
  {#if decodeFailed}
    <div class="card">
      <span class="ficon">{@html icons["paperclip"]}</span>
      <span class="fname">📎 Attachment (unavailable)</span>
    </div>
  {:else if showImage && xferState?.path}
    <img
      class="preview"
      src={convertFileSrc(xferState.path)}
      alt={filename}
      onerror={() => (imgBroken = true)}
    />
    <div class="card">
      <span class="fname">{filename}</span>
      {#if size !== undefined}<span class="fsize">{formatBytes(size)}</span>{/if}
      <div class="actions">
        <button type="button" onclick={doOpen} aria-label="Open">Open</button>
        <button type="button" onclick={doReveal} aria-label="Reveal in folder">Reveal</button>
      </div>
    </div>
  {:else}
    <div class="card">
      <span class="ficon">{@html iconGlyph}</span>
      <span class="fname">{filename}</span>
      {#if size !== undefined}<span class="fsize">{formatBytes(size)}</span>{/if}
      {#if isOutgoing && deliveryStatus}
        <DeliveryIcon status={deliveryStatus} />
      {/if}
      {#if complete}
        <div class="actions">
          <button type="button" onclick={doOpen} aria-label="Open">Open</button>
          <button type="button" onclick={doReveal} aria-label="Reveal in folder">Reveal</button>
        </div>
      {/if}
      {#if failed}
        <span class="failed">⚠️ {xferState?.reason ?? "Transfer failed"}</span>
      {/if}
    </div>
    {#if receiving}
      <div class="progress" class:indeterminate>
        {#if indeterminate}
          <span class="label">Downloading…</span>
        {:else}
          <div class="bar" style={`width:${pct}%`}></div>
          <span class="label">Downloading {pct}%</span>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .file-bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .file-bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .card { display: flex; align-items: center; gap: var(--s-2); flex-wrap: wrap; }
  .ficon :global(svg) { width: 20px; height: 20px; }
  .fname { font: var(--t-ui); word-break: break-word; }
  .fsize { color: var(--text-muted); font: var(--t-ui); }
  .file-bubble.outgoing .fsize { color: rgba(255, 255, 255, 0.7); }
  .preview { max-width: 100%; max-height: 320px; border-radius: 8px; display: block; margin-bottom: var(--s-1); }
  .actions { display: flex; gap: var(--s-1); }
  .actions button {
    padding: 2px var(--s-2);
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--bg-elevated);
    border-radius: 6px;
    font: var(--t-ui);
    cursor: pointer;
  }
  .progress { position: relative; margin-top: var(--s-1); height: 16px; background: var(--bg); border-radius: 4px; overflow: hidden; }
  .progress .bar { height: 100%; background: var(--accent); transition: width 0.2s; }
  .progress .label { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; font: var(--t-ui); color: var(--text); }
  .failed { color: var(--danger); font: var(--t-ui); }
</style>
