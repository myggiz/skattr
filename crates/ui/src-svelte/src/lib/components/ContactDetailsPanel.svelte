<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";
  import { rename, archive } from "$lib/stores/contacts";
  import { toast } from "$lib/stores/toast";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  interface Props {
    summary: ContactSummary;
  }
  let { summary }: Props = $props();

  let nickname = $state(summary.nickname ?? "");
  let confirmOpen = $state(false);

  let nicknameValid = $derived(
    nickname.trim().length > 0 && nickname.trim().length <= 64,
  );
  let pubkeyShort = $derived(
    `${summary.pubkey.slice(0, 8)}…${summary.pubkey.slice(-8)}`,
  );
  let onionShort = $derived(
    summary.onion.length > 20
      ? `${summary.onion.slice(0, 8)}…${summary.onion.slice(-8)}`
      : summary.onion,
  );

  async function copyToClipboard(value: string) {
    await navigator.clipboard.writeText(value);
    toast.show("Copied");
  }

  async function saveRename() {
    if (!nicknameValid) return;
    await rename(summary.pubkey, nickname.trim());
  }

  function openConfirm() {
    confirmOpen = true;
  }
  function closeConfirm() {
    confirmOpen = false;
  }
  async function doArchive() {
    await archive(summary.pubkey);
    confirmOpen = false;
  }
</script>

<section class="panel">
  <h3>Identity</h3>
  <button type="button" class="copyable" onclick={() => copyToClipboard(summary.pubkey)}>
    <span class="label">Pubkey</span>
    <span class="value mono">{pubkeyShort}</span>
  </button>
  <button type="button" class="copyable" onclick={() => copyToClipboard(summary.onion)}>
    <span class="label">Onion</span>
    <span class="value mono">{onionShort}</span>
  </button>

  <h3>Peer mailboxes</h3>
  <p class="empty">No mailboxes (peer mailbox projection lands in 2.F).</p>

  <h3>Rename</h3>
  <label>
    <span class="sr-only">Nickname</span>
    <input aria-label="Nickname" type="text" bind:value={nickname} maxlength="64" />
  </label>
  <div class="actions">
    <button type="button" onclick={saveRename} disabled={!nicknameValid}>Save</button>
  </div>

  <h3>Danger zone</h3>
  {#if !confirmOpen}
    <button type="button" class="archive" onclick={openConfirm}>Archive</button>
  {/if}
</section>

{#if confirmOpen}
  <ConfirmDialog
    title="Archive {summary.nickname ?? 'this contact'}?"
    body="{summary.nickname ?? 'They'} disappears from your contacts. Messages stay encrypted on disk; you can unarchive from Settings → Archived."
    confirmLabel="Archive"
    danger
    onConfirm={doArchive}
    onCancel={closeConfirm}
  />
{/if}

<style>
  .panel {
    padding: var(--s-3);
    background: var(--bg-elevated);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  h3 { font: var(--t-display); margin: var(--s-2) 0 0; }
  .copyable {
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: var(--bg);
    border: 1px solid var(--bg-elevated);
    border-radius: 4px;
    padding: var(--s-2);
    cursor: pointer;
    color: var(--text);
  }
  .label { color: var(--text-muted); font: var(--t-ui); }
  .value.mono { font-family: ui-monospace, monospace; }
  input[type="text"] { width: 100%; padding: 6px 8px; }
  .actions { display: flex; justify-content: flex-end; }
  .empty { color: var(--text-muted); font: var(--t-ui); }
  .archive { background: var(--danger); color: var(--text); border: none; padding: 8px 16px; cursor: pointer; }
  .sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; }
</style>
