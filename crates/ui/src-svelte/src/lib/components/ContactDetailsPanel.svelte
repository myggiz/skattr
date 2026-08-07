<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";
  import { rename, archive } from "$lib/stores/contacts";
  import { setContactMuted } from "$lib/stores/config";
  import { toast } from "$lib/stores/toast";
  import { copyText } from "$lib/clipboard";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  interface Props {
    summary: ContactSummary;
  }
  let { summary }: Props = $props();

  let contact = $state(summary);
  let nickname = $state(summary.nickname ?? "");
  let confirmOpen = $state(false);

  let nicknameValid = $derived(
    nickname.trim().length > 0 && nickname.trim().length <= 64,
  );
  let pubkeyShort = $derived(
    `${contact.pubkey.slice(0, 8)}…${contact.pubkey.slice(-8)}`,
  );
  let onionShort = $derived(
    contact.onion.length > 20
      ? `${contact.onion.slice(0, 8)}…${contact.onion.slice(-8)}`
      : contact.onion,
  );

  async function copyToClipboard(value: string) {
    await copyText(value);
    toast.show("Copied");
  }

  async function saveRename() {
    if (!nicknameValid) return;
    await rename(contact.pubkey, nickname.trim());
  }

  async function toggleMute() {
    const newVal = !contact.muted;
    try {
      await setContactMuted(contact.pubkey, newVal);
      contact = { ...contact, muted: newVal };
      toast.show(newVal ? "Notifications muted" : "Notifications unmuted");
    } catch (e) {
      toast.show(`Failed: ${e}`);
    }
  }

  function openConfirm() {
    confirmOpen = true;
  }
  function closeConfirm() {
    confirmOpen = false;
  }
  async function doArchive() {
    await archive(contact.pubkey);
    confirmOpen = false;
  }
</script>

<section class="panel">
  <div class="header">
    <h3>{contact.nickname ?? "Unnamed"}</h3>
    <button
      type="button"
      class="mute-toggle"
      onclick={toggleMute}
      title={contact.muted ? "Unmute notifications" : "Mute notifications"}
      aria-label={contact.muted ? "Unmute notifications" : "Mute notifications"}
    >
      {contact.muted ? "🔕" : "🔔"}
    </button>
  </div>

  <h3>Identity</h3>
  <button type="button" class="copyable" onclick={() => copyToClipboard(contact.pubkey)}>
    <span class="label">Pubkey</span>
    <span class="value mono">{pubkeyShort}</span>
  </button>
  <button type="button" class="copyable" onclick={() => copyToClipboard(contact.onion)}>
    <span class="label">Onion</span>
    <span class="value mono">{onionShort}</span>
  </button>

  <h3>Peer mailboxes</h3>
  {#if contact.peer_mailboxes && contact.peer_mailboxes.length > 0}
    <ul class="mailbox-list">
      {#each contact.peer_mailboxes as onion}
        <li class="mailbox-item">
          <code class="mailbox-onion">{onion.length > 24 ? onion.slice(0, 12) + "…" + onion.slice(-12) : onion}</code>
          <button type="button" class="copy-btn" onclick={() => copyToClipboard(onion)}>Copy</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">No mailboxes</p>
  {/if}

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
    title="Archive {contact.nickname ?? 'this contact'}?"
    body="{contact.nickname ?? 'They'} disappears from your contacts. Messages stay encrypted on disk; you can unarchive from Settings → Archived."
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
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-2);
  }
  h3 { font: var(--t-display); margin: var(--s-2) 0 0; }
  .header h3 { margin: 0; }
  .mute-toggle {
    background: transparent;
    border: none;
    cursor: pointer;
    font-size: 1.1em;
    padding: 4px;
    line-height: 1;
  }
  .mute-toggle:hover { opacity: 0.75; }
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
  .mailbox-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }
  .mailbox-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-2);
    background: var(--bg);
    border: 1px solid var(--bg-elevated);
    border-radius: 4px;
    padding: var(--s-2);
  }
  .mailbox-onion {
    font-family: ui-monospace, monospace;
    font-size: 0.85em;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .copy-btn {
    background: transparent;
    border: 1px solid var(--bg-elevated);
    border-radius: 4px;
    padding: 2px 8px;
    cursor: pointer;
    color: var(--text-muted);
    font: var(--t-ui);
    flex-shrink: 0;
  }
  .copy-btn:hover { color: var(--text); }
</style>
