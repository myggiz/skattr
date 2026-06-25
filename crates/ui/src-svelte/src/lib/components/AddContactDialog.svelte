<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { ipcClient } from "$lib/ipc/tauri";
  import { refreshContacts } from "$lib/stores/contacts";
  import { errorMessage } from "$lib/ipc/errors";
  import jsQR from "jsqr";
  import { onDestroy } from "svelte";

  interface Props {
    onClose: () => void;
    /** Pre-fill the paste tab with this URL (used by deep-link handler). */
    initialUrl?: string;
  }
  let { onClose, initialUrl = "" }: Props = $props();

  type Tab = "paste" | "scan";
  let tab = $state<Tab>("paste");

  // Paste tab state
  let url = $state(initialUrl);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Scan tab state
  let scanStream = $state<MediaStream | null>(null);
  let scanError = $state<string | null>(null);
  let scanPreview = $state<string | null>(null);
  let video = $state<HTMLVideoElement | null>(null);
  let canvas = $state<HTMLCanvasElement | null>(null);
  let rafHandle = $state<number | null>(null);

  async function submit() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const resp = await ipcClient.request({
        cmd: "add_contact",
        invite_url: url.trim(),
      } as any);
      if (resp.resp === "err") {
        const d = resp.data;
        if (d.err === "daemon" && d.data.kind === "delivery_timeout") {
          error = "Couldn't reach your contact. First contact needs both of you online at the same time — try again when they're back online.";
        } else {
          error = errorMessage(d);
        }
        return; // keep the dialog open for retry
      }
      if (resp.resp !== "ok") {
        error = "Something went wrong.";
        return;
      }
      await refreshContacts();
      onClose();
    } catch (e) {
      error = `${e}`;
    } finally {
      busy = false;
    }
  }

  async function startScan() {
    scanError = null;
    scanPreview = null;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ video: true });
      scanStream = stream;
      // attach to video element after next tick
      await new Promise((r) => setTimeout(r, 0));
      if (video) {
        video.srcObject = stream;
        await video.play();
        scanLoop();
      }
    } catch (e) {
      scanError = `Camera access denied: ${e}`;
    }
  }

  function scanLoop() {
    if (!video || !canvas || !scanStream) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    canvas.width = video.videoWidth || 320;
    canvas.height = video.videoHeight || 240;
    ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
    const imageData = ctx.getImageData(0, 0, canvas.width, canvas.height);
    const result = jsQR(imageData.data, canvas.width, canvas.height);
    if (result && result.data.startsWith("skattr://")) {
      scanPreview = result.data;
      stopScan();
      return;
    }
    rafHandle = requestAnimationFrame(scanLoop);
  }

  function stopScan() {
    if (rafHandle !== null) {
      cancelAnimationFrame(rafHandle);
      rafHandle = null;
    }
    if (scanStream) {
      scanStream.getTracks().forEach((t) => t.stop());
      scanStream = null;
    }
  }

  async function confirmScan() {
    if (!scanPreview) return;
    url = scanPreview;
    tab = "paste";
    scanPreview = null;
    await submit();
  }

  $effect(() => {
    if (tab === "scan") {
      startScan();
    } else {
      stopScan();
    }
    return stopScan;
  });

  onDestroy(stopScan);
</script>

<div class="overlay" role="dialog" aria-modal="true">
  <div class="dialog">
    <h2>New contact</h2>
    <div class="tabs" role="tablist">
      <button type="button" role="tab" aria-selected={tab === "paste"} onclick={() => (tab = "paste")}>Paste</button>
      <button type="button" role="tab" aria-selected={tab === "scan"} onclick={() => (tab = "scan")}>Scan</button>
    </div>

    {#if tab === "paste"}
      <textarea placeholder="skattr://invite/v1#…" bind:value={url} rows="4"></textarea>
      {#if error}<p class="error">{error}</p>{/if}
      <div class="actions">
        <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
        <button type="button" onclick={submit} disabled={busy || url.trim().length === 0}>
          {busy ? "Adding…" : "Add contact"}
        </button>
      </div>
    {:else}
      {#if scanError}
        <p class="error">{scanError}</p>
        <div class="actions">
          <button type="button" onclick={() => (tab = "paste")}>Switch to Paste</button>
          <button type="button" onclick={onClose}>Cancel</button>
        </div>
      {:else if scanPreview}
        <p>Found invite link:</p>
        <code class="url">{scanPreview}</code>
        <div class="actions">
          <button type="button" onclick={() => { scanPreview = null; startScan(); }}>Re-scan</button>
          <button type="button" onclick={confirmScan} disabled={busy}>
            {busy ? "Adding…" : "Use this link"}
          </button>
        </div>
      {:else}
        <video bind:this={video} muted playsinline></video>
        <canvas bind:this={canvas} style="display:none"></canvas>
        <p class="hint">Point camera at a Skattr QR code…</p>
        <div class="actions">
          <button type="button" onclick={onClose}>Cancel</button>
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.6); display: grid; place-items: center; z-index: 900; }
  .dialog { background: var(--bg-elevated); color: var(--text); padding: var(--s-3); border-radius: 8px; max-width: 520px; width: 90vw; }
  h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  .tabs { display: flex; gap: var(--s-2); margin-bottom: var(--s-3); border-bottom: 1px solid var(--bg); }
  .tabs button[aria-selected="true"] { border-bottom: 2px solid var(--accent); }
  textarea { width: 100%; padding: var(--s-2); resize: vertical; font: var(--t-ui); }
  .error { color: var(--danger); margin: var(--s-2) 0; }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); margin-top: var(--s-3); }
  .url { display: block; padding: var(--s-2); background: var(--bg); word-break: break-all; }
  .hint { color: var(--text-muted); font: var(--t-ui); margin: var(--s-2) 0; }
  video { width: 100%; max-width: 360px; display: block; margin: 0 auto; }
</style>
