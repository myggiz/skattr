<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";

  import TorPill from "$lib/components/TorPill.svelte";
  import ContactRow from "$lib/components/ContactRow.svelte";
  import VirtualMessageList from "$lib/components/VirtualMessageList.svelte";
  import { contacts, refreshContacts } from "$lib/stores/contacts";
  import { conversation, openConversation, appendMessage } from "$lib/stores/conversation";
  import { torStatus } from "$lib/stores/tor_status";
  import { ipcClient } from "$lib/ipc/tauri";
  import type { PublicKey } from "$lib/ipc/types";

  onMount(async () => {
    // If no vault exists yet, go to first-run to initialise identity.
    const exists = await invoke<boolean>("vault_exists");
    if (!exists) {
      goto("/first-run");
    }
    // If vault exists we are already unlocked (Bootstrap.svelte called goto("/")).
    // Stay on "/" and show the main shell.
  });

  async function selectContact(pubkey: PublicKey) {
    await openConversation(pubkey);
  }

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
        onclick={() => selectContact(c.pubkey)}
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
    <VirtualMessageList items={$conversation.messages} />
  </main>
</div>

<style>
  .shell { display: grid; grid-template-columns: 280px 1fr; height: 100vh; }
  .rail { background: var(--bg); border-right: 1px solid var(--bg-elevated); overflow-y: auto; }
  .pane { display: flex; flex-direction: column; background: var(--bg); }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--s-3);
    border-bottom: 1px solid var(--bg-elevated);
  }
  .title { font: var(--t-display); }
</style>
