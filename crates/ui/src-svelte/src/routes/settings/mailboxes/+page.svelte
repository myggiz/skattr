<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";
  import type { MailboxSummary } from "$lib/ipc/types";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import { toast } from "$lib/stores/toast";

  let mailboxes = $state<MailboxSummary[]>([]);
  let showAdd = $state(false);
  let pendingOnion = $state("");
  let confirmRemove = $state<bigint | null>(null);
  let unsubscribe: (() => void) | null = null;

  async function refresh() {
    try {
      const resp = await ipcClient.request({ cmd: "list_mailboxes" });
      const result = unwrapOk(resp);
      if (result.result !== "mailboxes") {
        throw new Error(`unexpected reply: ${result.result}`);
      }
      mailboxes = result.data;
    } catch (e) {
      toast.show(`Failed to list mailboxes: ${e}`);
    }
  }

  async function addMailbox() {
    const onion = pendingOnion.trim();
    if (!onion) return;
    try {
      await ipcClient.request({ cmd: "add_mailbox", onion });
      toast.show("Mailbox added");
      pendingOnion = "";
      showAdd = false;
      await refresh();
    } catch (e) {
      toast.show(`Add failed: ${e}`);
    }
  }

  async function removeMailbox(id: bigint) {
    confirmRemove = null;
    try {
      await ipcClient.request({ cmd: "remove_mailbox", id });
      toast.show("Mailbox removed");
      await refresh();
    } catch (e) {
      toast.show(`Remove failed: ${e}`);
    }
  }

  onMount(async () => {
    await refresh();
    try {
      unsubscribe = await ipcClient.subscribe(
        { filter: "mailboxes" },
        (evt) => {
          if (evt.event === "mailbox_status_changed") {
            const { mailbox_id, status } = evt.data;
            mailboxes = mailboxes.map((m) =>
              m.id === mailbox_id ? { ...m, status } : m,
            );
          }
        },
      );
    } catch {
      // Live updates unavailable; manual refresh after add/remove still works.
    }
  });

  onDestroy(() => {
    unsubscribe?.();
  });

  function fmtDate(ts: bigint): string {
    return new Date(Number(ts) * 1000).toLocaleDateString();
  }

  function shortOnion(s: string): string {
    return s.length > 28 ? s.slice(0, 14) + "…" + s.slice(-14) : s;
  }
</script>

<h1>Mailboxes</h1>

{#if mailboxes.length === 0}
  <p class="muted">No mailboxes registered.</p>
{:else}
  <ul class="mailbox-list">
    {#each mailboxes as m (m.id)}
      <li>
        <code title={m.onion}>{shortOnion(m.onion)}</code>
        <span class="status status-{m.status}">{m.status}</span>
        <span class="ts">since {fmtDate(m.registered_at)}</span>
        <button type="button" onclick={() => (confirmRemove = m.id)}>Remove</button>
      </li>
    {/each}
  </ul>
{/if}

<div class="add-row">
  <button type="button" onclick={() => (showAdd = true)}>Add mailbox</button>
</div>

{#if showAdd}
  <div
    class="modal-overlay"
    onclick={() => (showAdd = false)}
    onkeydown={(e) => e.key === "Escape" && (showAdd = false)}
    role="dialog"
    aria-modal="true"
    aria-label="Add mailbox"
    tabindex="-1"
  >
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      role="presentation"
    >
      <h2>Add mailbox</h2>
      <label>
        Onion address
        <input
          type="text"
          bind:value={pendingOnion}
          placeholder="abc…xyz.onion"
          autocomplete="off"
          spellcheck={false}
        />
      </label>
      <div class="actions">
        <button type="button" onclick={() => (showAdd = false)}>Cancel</button>
        <button type="button" onclick={addMailbox} disabled={!pendingOnion.trim()}>Add</button>
      </div>
    </div>
  </div>
{/if}

{#if confirmRemove !== null}
  <ConfirmDialog
    title="Remove mailbox?"
    body="Pending deposits to this mailbox may be lost."
    confirmLabel="Remove"
    onConfirm={() => removeMailbox(confirmRemove!)}
    onCancel={() => (confirmRemove = null)}
  />
{/if}

<style>
  h1 { font: var(--t-display); margin: 0 0 var(--s-3); }

  .muted { color: var(--text-muted, #888); }

  .mailbox-list {
    list-style: none;
    padding: 0;
    margin: var(--s-2, 12px) 0;
  }

  .mailbox-list li {
    display: flex;
    gap: var(--s-2, 12px);
    align-items: center;
    padding: var(--s-1, 6px) 0;
    border-bottom: 1px solid var(--border, #2a2a2a);
    flex-wrap: wrap;
  }

  code {
    font-family: monospace;
    background: var(--bg, #111);
    padding: 2px 6px;
    border-radius: 3px;
    flex: 1;
    min-width: 0;
  }

  .status {
    font-size: 0.75em;
    padding: 2px 8px;
    border-radius: 8px;
    background: var(--bg, #111);
    color: var(--text-muted, #888);
    white-space: nowrap;
  }

  .status-reachable,
  .status-authenticated {
    color: var(--accent, #6c6);
  }

  .status-unreachable,
  .status-removed {
    color: var(--danger, #d33);
  }

  .status-rate_limited,
  .status-pending_removal {
    color: var(--warn, #fa0);
  }

  .ts {
    color: var(--text-muted, #888);
    font-size: 0.85em;
    white-space: nowrap;
  }

  .add-row {
    margin-top: var(--s-3, 16px);
  }

  button {
    padding: 8px 16px;
    cursor: pointer;
  }

  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal {
    background: var(--bg-elevated, #1a1a1a);
    padding: var(--s-3, 16px);
    border-radius: 8px;
    min-width: 360px;
    display: flex;
    flex-direction: column;
    gap: var(--s-2, 12px);
  }

  .modal h2 {
    margin: 0;
    font: var(--t-display);
  }

  .modal label {
    display: flex;
    flex-direction: column;
    gap: var(--s-1, 6px);
    font: var(--t-ui);
    color: var(--text-muted, #888);
  }

  .modal input {
    padding: 8px 10px;
    border-radius: 4px;
    border: 1px solid var(--border, #2a2a2a);
    background: var(--bg, #111);
    color: var(--text, #eee);
    font-family: monospace;
    font-size: 0.9em;
  }

  .actions {
    display: flex;
    gap: var(--s-2, 12px);
    justify-content: flex-end;
  }
</style>
