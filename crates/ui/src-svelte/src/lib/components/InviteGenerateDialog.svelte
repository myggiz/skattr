<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { ipcClient } from "$lib/ipc/tauri";
  import { renderInviteQr } from "$lib/stores/qr";
  import { toast } from "$lib/stores/toast";
  import { copyText } from "$lib/clipboard";

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  type Step = "form" | "result";
  let step = $state<Step>("form");
  let nickname = $state("");
  let ttlSecs = $state<number>(86400);
  let url = $state<string | null>(null);
  let qrSvg = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  const TTL_OPTIONS = [
    { label: "1 hour", secs: 3600 },
    { label: "6 hours", secs: 21600 },
    { label: "24 hours", secs: 86400 },
    { label: "7 days", secs: 604800 },
  ];

  async function generate() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const trimmed = nickname.trim();
      const resp = await ipcClient.request({
        cmd: "create_invite",
        nickname: trimmed.length === 0 ? null : trimmed,
        ttl_secs: ttlSecs,
      } as any);
      if (resp.resp !== "ok") {
        error = "Failed to create invite.";
        return;
      }
      const data = resp.data;
      if (data?.result !== "invite_created") {
        error = "Unexpected response from daemon.";
        return;
      }
      url = data.data.url as string;
      qrSvg = await renderInviteQr(url);
      step = "result";
    } catch (e) {
      error = `${e}`;
    } finally {
      busy = false;
    }
  }

  async function copyUrl() {
    if (!url) return;
    try {
      await copyText(url);
      toast.show("Copied");
    } catch (e) {
      toast.show(`Copy failed: ${e}`);
    }
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !busy) onClose();
  }
</script>

<svelte:window onkeydown={handleKey} />

<div class="overlay" role="dialog" aria-modal="true">
  <div class="dialog">
    {#if step === "form"}
      <h2>Generate invite</h2>
      <label>
        Nickname (optional)
        <input type="text" bind:value={nickname} maxlength="64" />
      </label>
      <fieldset>
        <legend>Expires in</legend>
        {#each TTL_OPTIONS as opt}
          <label>
            <input
              type="radio"
              name="ttl"
              value={opt.secs}
              checked={ttlSecs === opt.secs}
              onchange={() => (ttlSecs = opt.secs)}
            />
            {opt.label}
          </label>
        {/each}
      </fieldset>
      {#if error}<p class="error">{error}</p>{/if}
      <div class="actions">
        <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
        <button type="button" onclick={generate} disabled={busy}>
          {busy ? "Generating…" : "Generate"}
        </button>
      </div>
    {:else}
      <h2>Invite ready</h2>
      <code class="url">{url}</code>
      {#if qrSvg}<div class="qr">{@html qrSvg}</div>{/if}
      <div class="actions">
        <button type="button" onclick={copyUrl}>Copy URL</button>
        <button type="button" onclick={onClose}>Done</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid; place-items: center;
    z-index: 900;
  }
  .dialog {
    background: var(--bg-elevated); color: var(--text);
    padding: var(--s-3); border-radius: 8px;
    max-width: 520px; width: 90vw;
    max-height: 90vh; overflow-y: auto;
  }
  h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  label { display: block; margin: var(--s-2) 0; }
  input[type="text"] { width: 100%; padding: 6px 8px; }
  fieldset { border: 1px solid var(--bg); padding: var(--s-2); margin: var(--s-2) 0; }
  .error { color: var(--danger); margin: var(--s-2) 0; }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); margin-top: var(--s-3); }
  .url { display: block; padding: var(--s-2); background: var(--bg); word-break: break-all; }
  .qr { margin: var(--s-3) auto; max-width: 240px; }
</style>
