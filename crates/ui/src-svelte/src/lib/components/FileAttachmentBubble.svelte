<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { save, ask } from "@tauri-apps/plugin-dialog";
  import type { MessageRecord } from "$lib/ipc/types";
  import type { OptimisticMessage } from "$lib/stores/conversation";
  import { attachments, applyManifest, markAvailable, markRetrying } from "$lib/stores/attachments";
  import { delivery, deliveryToIconStatus, hex16ToString } from "$lib/stores/delivery";
  import { decodeManifestMemo, mimeIconName, formatBytes } from "$lib/attachments";
  import type { ManifestSummary } from "$lib/attachments";
  import { icons } from "$lib/icons";
  import { toast } from "$lib/stores/toast";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";
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

  // Decode the manifest once per message id; on success seed the store's
  // static fields so the bubble can render filename/size even if no transfer
  // events have arrived yet.
  $effect(() => {
    if (record.kind.kind !== "file") return;
    const fileKind = record.kind;
    // An optimistic outgoing placeholder carries an empty manifest (the real
    // bytes are not known until SendFile returns). There is nothing to decode
    // yet — skip, so the bubble falls through to the file card showing the
    // picked filename/size rather than the "unavailable" decode-failure card.
    if ((fileKind.manifest as unknown as number[]).length === 0) return;
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

  // Rehydrate after restart: the transfer store is session-scoped. Ask the
  // daemon whether this completed attachment is decryptable and, if so, enable
  // Open/Save. No plaintext is produced by this query.
  $effect(() => {
    if (isOutgoing || !summary || xferState?.available) return;
    const s = summary;
    const aid = s.attachment_id; // hex string
    ipcClient
      .request({ cmd: "attachment_available", attachment_id: aid } as any)
      .then((resp) => {
        if (resp.resp !== "ok") return;
        const result = resp.data;
        if (result.result === "attachment_availability" && result.data.available) {
          markAvailable(aid, { filename: s.filename, mime: s.mime, size: s.total_size });
        }
      })
      .catch(() => {});
  });

  // Display fields: prefer decoded manifest, fall back to optimistic send info.
  let filename = $derived(summary?.filename ?? optimisticName ?? "");
  let mime = $derived(summary?.mime);
  let size = $derived(summary?.total_size ?? optimisticSize);

  let receiving = $derived(!isOutgoing && xferState?.status === "receiving");
  let complete = $derived(!isOutgoing && (xferState?.status === "complete" || xferState?.available === true));
  let failed = $derived(!isOutgoing && xferState?.status === "failed");
  let retrying = $derived(!isOutgoing && xferState?.retrying === true);

  // Sender side: chunk-transfer state supersedes the manifest-ack delivery
  // icon. The manifest is MLS-acked before any chunk moves, so the icon alone
  // must never read as "the file arrived" (#114).
  let sendComplete = $derived(
    isOutgoing &&
      (xferState?.status === "complete" ||
        (xferState !== undefined && xferState.total > 0 && xferState.received >= xferState.total)),
  );
  // "queued" is the decode-time seed state (applyManifest fires on mount for
  // both directions, before any chunk moves) — it must not read as sending.
  let sending = $derived(
    isOutgoing && xferState !== undefined && xferState.status !== "queued" && !sendComplete,
  );
  let sentPct = $derived(
    xferState && xferState.total > 0 ? `${xferState.received}/${xferState.total}` : null,
  );

  let pct = $derived(
    xferState && xferState.total > 0 ? Math.round((xferState.received / xferState.total) * 100) : 0,
  );
  let indeterminate = $derived(receiving && (!xferState || xferState.total === 0));

  // A file's manifest ack means the request arrived, not the file. Never show
  // the delivered checkmark until the chunk transfer itself completed (#114).
  function capFileDelivery(
    s: "pending" | "sent" | "delivered" | "failed",
  ): "pending" | "sent" | "delivered" | "failed" {
    return s === "delivered" ? "sent" : s;
  }

  let deliveryStatus = $derived(
    isOutgoing ? capFileDelivery(deliveryToIconStatus($delivery.get(hex16ToString(record.message_id)))) : null,
  );

  // Returns the managed-cache plaintext path, or throws.
  async function decryptToCache(): Promise<string> {
    const resp = await ipcClient.request({ cmd: "open_attachment", attachment_id: aidHex } as any);
    const result = unwrapOk(resp); // throws on err
    if (result.result !== "attachment_decrypted") throw new Error("unexpected result");
    return result.data.path;
  }

  async function doOpen() {
    if (!aidHex) return;
    // Decrypt first; surface a clear error on failure rather than confusing it
    // with an opener failure (they have different causes and different remedies).
    let path: string;
    try {
      path = await decryptToCache();
    } catch {
      toast.show("Couldn't open the attachment — it may be corrupted or unavailable.");
      return;
    }
    // Decryption succeeded; attempt to open with the system handler.
    try {
      await invoke("open_file", { path });
    } catch {
      const showFolder = await ask(
        "Your system doesn't have an app set to open this type of file. Open its folder instead, so you can open it yourself?",
        { title: "Can't open file", kind: "warning" },
      );
      if (showFolder) await doReveal();
    }
  }

  async function doReveal() {
    if (!aidHex) return;
    try {
      const path = await decryptToCache();
      await invoke("reveal_in_folder", { path });
    } catch {
      toast.show("Couldn't open the folder");
    }
  }

  async function doSave() {
    if (!aidHex) return;
    const dest = await save({ defaultPath: filename || undefined });
    if (!dest) return; // cancelled
    try {
      const resp = await ipcClient.request({
        cmd: "save_attachment",
        attachment_id: aidHex,
        dest_path: dest,
      } as any);
      if (resp.resp !== "ok") throw new Error("save failed");
      toast.show(`Saved to ${dest}`);
    } catch {
      toast.show("Couldn't save the file");
    }
  }

  // #144: ask the daemon to re-arm a failed transfer. Best-effort by nature —
  // the sender drops its staged chunks once a deposit sweep or a peer ack
  // completes, so this can still come back as a fresh AttachmentFailed. Say
  // "asked", not "downloading", and let the events tell the real story.
  async function doRetry() {
    // Bind to a const so the guard narrows it to string for the typed request
    // below — `aidHex` is a reactive `let`, which TS will not narrow across the
    // await.
    const aid = aidHex;
    if (!aid) return;
    try {
      const resp = await ipcClient.request({ cmd: "retry_attachment", attachment_id: aid });
      if (resp.resp !== "ok") throw new Error("retry rejected");
      markRetrying(aid);
      toast.show("Retrying — this only works while the sender still has the file.");
    } catch {
      toast.show("Couldn't retry this transfer.");
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
  {:else}
    <div class="card">
      <span class="ficon">{@html iconGlyph}</span>
      <span class="fname">{filename}</span>
      {#if size !== undefined}<span class="fsize">{formatBytes(size)}</span>{/if}
      {#if isOutgoing && sendComplete}
        <span class="delivered">Delivered</span>
      {:else if isOutgoing && !sending && deliveryStatus}
        <DeliveryIcon status={deliveryStatus} />
      {/if}
      {#if complete}
        <div class="actions">
          <button type="button" onclick={doOpen} aria-label="Open">Open</button>
          <button type="button" onclick={doSave} aria-label="Save decrypted file">Save…</button>
        </div>
      {/if}
      {#if failed}
        <span class="failed">⚠️ {xferState?.reason ?? "Transfer failed"}</span>
        <div class="actions">
          <button type="button" onclick={doRetry} aria-label="Retry transfer">Retry</button>
        </div>
      {:else if retrying}
        <span class="waiting">Retrying — waiting for the sender…</span>
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
    {#if sending}
      <div class="progress">
        {#if sentPct}
          <span class="label">Sending {sentPct}</span>
        {:else}
          <span class="label">Sending…</span>
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
  .waiting { color: var(--text-muted); font: var(--t-ui); }
  .delivered { color: var(--text-muted); font: var(--t-ui); }
  .file-bubble.outgoing .delivered { color: rgba(255, 255, 255, 0.7); }
</style>
