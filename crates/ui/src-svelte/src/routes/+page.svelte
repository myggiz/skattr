<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";

  import TorPill from "$lib/components/TorPill.svelte";
  import ContactRow from "$lib/components/ContactRow.svelte";
  import ContactDetailsPanel from "$lib/components/ContactDetailsPanel.svelte";
  import VirtualMessageList from "$lib/components/VirtualMessageList.svelte";
  import Composer from "$lib/components/Composer.svelte";
  import InviteGenerateDialog from "$lib/components/InviteGenerateDialog.svelte";
  import AddContactDialog from "$lib/components/AddContactDialog.svelte";
  import Toast from "$lib/components/Toast.svelte";
  import { contacts, refreshContacts, expandedPubkey, toggleExpanded } from "$lib/stores/contacts";
  import { conversation, openConversationFromSummary, appendMessage } from "$lib/stores/conversation";
  import { torStatus } from "$lib/stores/tor_status";
  import { recordDeliveryStatus, hex16ToString } from "$lib/stores/delivery";
  import { deepLinkInviteUrl } from "$lib/stores/deepLink";
  import { ipcClient } from "$lib/ipc/tauri";
  import type { ContactSummary, PublicKey } from "$lib/ipc/types";

  let inviteOpen = $state(false);
  let addOpen = $state(false);
  // URL pre-filled when the dialog is opened via a skattr:// deep-link.
  let addInitialUrl = $state("");

  onMount(async () => {
    // If no vault exists yet, go to first-run to initialise identity.
    const exists = await invoke<boolean>("vault_exists");
    if (!exists) {
      goto("/first-run");
      return;
    }
    // If vault exists we are already unlocked (Bootstrap.svelte called goto("/")).
    // Stay on "/" and show the main shell; refresh the contact list.
    await refreshContacts();
  });

  async function selectContact(summary: ContactSummary) {
    await openConversationFromSummary(summary);
  }

  // Active ContactSummary lookup keyed by the conversation's current contact.
  let activeSummary = $derived(
    $conversation.contact === null
      ? undefined
      : $contacts.find((c) => c.pubkey === $conversation.contact),
  );

  let composerDisabled = $derived(
    activeSummary === undefined || activeSummary.group_state !== "active",
  );

  let disabledReason = $derived(
    activeSummary === undefined
      ? "Select a contact"
      : activeSummary.group_state === "corrupt"
        ? "Conversation unavailable"
        : activeSummary.group_state === "pending_join"
          ? "Joining group…"
          : activeSummary.group_state === null || activeSummary.group_state === undefined
            ? "Setting up conversation…"
            : undefined,
  );

  // Subscribe to the deep-link store; open the Add-Contact dialog with the
  // URL pre-filled whenever a skattr://invite/v1#… deep-link arrives.
  onMount(() => {
    const unsubDeepLink = deepLinkInviteUrl.subscribe((url) => {
      if (url !== null) {
        addInitialUrl = url;
        addOpen = true;
        // Clear the store so a second deep-link can re-trigger the dialog.
        deepLinkInviteUrl.set(null);
      }
    });
    return unsubDeepLink;
  });

  // Subscribe to events on mount; update stores.
  onMount(() => {
    let unsub: (() => void) | null = null;
    (async () => {
      unsub = await ipcClient.subscribe({ filter: "all" }, (e) => {
        // Event is adjacent-tagged: { event: "...", data: ... }
        if (e.event === "tor_status_changed") {
          torStatus.set(e.data);
        } else if (e.event === "message_received") {
          appendMessage(e.data.record);
        } else if (e.event === "delivery_status_changed") {
          recordDeliveryStatus(hex16ToString(e.data.message), e.data.status);
        }
      });
    })();
    return () => {
      if (unsub) unsub();
    };
  });
</script>

<div class="shell">
  <aside class="rail">
    <div class="rail-header">
      <button type="button" class="rail-btn" onclick={() => (inviteOpen = true)}>
        Generate invite
      </button>
      <button type="button" class="rail-btn" onclick={() => (addOpen = true)}>
        + Add
      </button>
      <button
        type="button"
        class="rail-btn"
        onclick={() => goto('/settings/identity')}
        title="Settings"
        aria-label="Open settings"
      >
        ⚙
      </button>
    </div>
    {#each $contacts as c}
      <ContactRow
        summary={c}
        active={$conversation.contact === c.pubkey}
        expanded={$expandedPubkey === c.pubkey}
        onclick={() => selectContact(c)}
        onToggleExpanded={() => toggleExpanded(c.pubkey)}
      />
      {#if $expandedPubkey === c.pubkey}
        <ContactDetailsPanel summary={c} />
      {/if}
    {/each}
  </aside>
  <main class="pane">
    <header>
      <span class="title">{
        $contacts.find((c) => c.pubkey === $conversation.contact)?.nickname
        ?? "Select a contact"
      }</span>
      <TorPill />
    </header>
    {#if $conversation.contact !== null}
      <VirtualMessageList items={$conversation.messages} />
      <Composer contact={$conversation.contact} disabled={composerDisabled} {disabledReason} />
    {:else}
      <p class="empty">Select a contact</p>
    {/if}
  </main>
</div>

{#if inviteOpen}
  <InviteGenerateDialog onClose={() => (inviteOpen = false)} />
{/if}
{#if addOpen}
  <AddContactDialog
    onClose={() => { addOpen = false; addInitialUrl = ""; }}
    initialUrl={addInitialUrl}
  />
{/if}

<Toast />

<style>
  .shell { display: grid; grid-template-columns: 280px 1fr; grid-template-rows: 100vh; height: 100vh; overflow: hidden; }
  .rail { background: var(--bg); border-right: 1px solid var(--bg-elevated); overflow-y: auto; }
  .rail-header {
    display: flex;
    gap: var(--s-1);
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--bg-elevated);
  }
  .rail-btn {
    flex: 1;
    padding: 6px var(--s-2);
    background: var(--bg-elevated);
    border: none;
    border-radius: 4px;
    color: var(--text);
    font: var(--t-ui);
    cursor: pointer;
  }
  .rail-btn:hover { background: var(--accent); color: var(--bg); }
  .pane { display: flex; flex-direction: column; background: var(--bg); height: 100%; }
  .pane :global(.list) { flex: 1; min-height: 0; }
  .empty { padding: var(--s-3); color: var(--fg-dim, #888); margin: auto; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--s-3);
    border-bottom: 1px solid var(--bg-elevated);
  }
  .title { font: var(--t-display); }
</style>
