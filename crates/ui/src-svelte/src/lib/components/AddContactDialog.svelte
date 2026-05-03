<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { ipcClient } from "$lib/ipc/tauri";
  import { refreshContacts } from "$lib/stores/contacts";

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  type Tab = "paste" | "scan";
  let tab = $state<Tab>("paste");

  let url = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function submit() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      const resp = await ipcClient.request({
        cmd: "add_contact",
        invite_url: url.trim(),
      } as any);
      if (resp.resp !== "ok") {
        error = "Failed to add contact.";
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
      <p>Scan tab — coming in next task.</p>
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
</style>
