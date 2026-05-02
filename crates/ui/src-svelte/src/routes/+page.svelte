<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";

  import TorPill from "$lib/components/TorPill.svelte";
  import ContactRow from "$lib/components/ContactRow.svelte";
  import VirtualMessageList from "$lib/components/VirtualMessageList.svelte";
  import Composer from "$lib/components/Composer.svelte";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { conversation, openConversationFromSummary, appendMessage } from "$lib/stores/conversation";
  import { torStatus } from "$lib/stores/tor_status";
  import { ipcClient } from "$lib/ipc/tauri";
  import type { ContactSummary, PublicKey } from "$lib/ipc/types";

  onMount(async () => {
    // If no vault exists yet, go to first-run to initialise identity.
    const exists = await invoke<boolean>("vault_exists");
    if (!exists) {
      goto("/first-run");
    }
    // If vault exists we are already unlocked (Bootstrap.svelte called goto("/")).
    // Stay on "/" and show the main shell.
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
    activeSummary === undefined ||
      activeSummary.group_state === "corrupt" ||
      activeSummary.group_state === "pending_join",
  );

  let disabledReason = $derived(
    activeSummary === undefined
      ? "Select a contact"
      : activeSummary.group_state === "corrupt"
        ? "Conversation unavailable"
        : activeSummary.group_state === "pending_join"
          ? "Joining group…"
          : undefined,
  );

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
    {#each $contacts as c}
      <ContactRow
        summary={c}
        active={$conversation.contact === c.pubkey}
        onclick={() => selectContact(c)}
      />
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

<style>
  .shell { display: grid; grid-template-columns: 280px 1fr; height: 100vh; }
  .rail { background: var(--bg); border-right: 1px solid var(--bg-elevated); overflow-y: auto; }
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
