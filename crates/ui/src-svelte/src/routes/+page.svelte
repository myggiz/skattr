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
  import { applyProgress, applyReceived, applyFailed } from "$lib/stores/attachments";
  import { deepLinkInviteUrl } from "$lib/stores/deepLink";
  import { ipcClient } from "$lib/ipc/tauri";
  import { connection, handleStreamClosed } from "$lib/stores/connection";
  import { pubkeyEq } from "$lib/pubkey";
  import { listen } from "@tauri-apps/api/event";
  import type { ContactSummary, PublicKey } from "$lib/ipc/types";

  let inviteOpen = $state(false);
  let addOpen = $state(false);
  // URL pre-filled when the dialog is opened via a skattr:// deep-link.
  let addInitialUrl = $state("");
  // Set to true once the first onMount confirms the daemon is running.
  // Guards the subscription onMounts so they don't attempt to subscribe
  // when we're about to redirect to /first-run (daemon not yet started).
  let daemonConfirmed = $state(false);

  onMount(async () => {
    // If no vault exists yet, go to first-run to initialise identity.
    const exists = await invoke<boolean>("vault_exists");
    if (!exists) {
      goto("/first-run");
      return;
    }
    // A vault exists, but that alone doesn't mean we're usable: on a fresh
    // launch (relaunch with an existing vault) we have NOT unlocked and the
    // in-process daemon is NOT running yet. Only stay on the main shell if the
    // daemon actually started this session (set after Bootstrap → goto("/")).
    // Otherwise route to first-run to unlock, which starts the daemon and
    // returns here.
    const running = await invoke<boolean>("daemon_running");
    if (!running) {
      goto("/first-run");
      return;
    }
    daemonConfirmed = true;
    await refreshContacts();
  });

  async function selectContact(summary: ContactSummary) {
    await openConversationFromSummary(summary);
  }

  // Active ContactSummary lookup keyed by the conversation's current contact.
  let activeSummary = $derived(
    $conversation.contact === null
      ? undefined
      : $contacts.find((c) => pubkeyEq(c.pubkey, $conversation.contact)),
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
  // `reSubscribe` is extracted so it can be re-invoked on IPC stream death.
  // Only called when `daemonConfirmed` — not when redirecting to /first-run.
  let unsub: (() => void) | null = null;
  async function reSubscribe(): Promise<void> {
    if (unsub) { unsub(); unsub = null; }
    unsub = await ipcClient.subscribe({ filter: "all" }, (e) => {
      // Event is adjacent-tagged: { event: "...", data: ... }
      if (e.event === "tor_status_changed") {
        torStatus.set(e.data);
      } else if (e.event === "message_received") {
        appendMessage(e.data.record);
      } else if (e.event === "delivery_status_changed") {
        recordDeliveryStatus(hex16ToString(e.data.message), e.data.status);
      } else if (e.event === "attachment_progress") {
        applyProgress(hex16ToString(e.data.attachment_id), e.data.received, e.data.total);
      } else if (e.event === "attachment_received") {
        applyReceived(hex16ToString(e.data.attachment_id), {
          filename: e.data.filename,
          mime: e.data.mime,
          size: Number(e.data.size),
        });
      } else if (e.event === "attachment_failed") {
        applyFailed(hex16ToString(e.data.attachment_id), e.data.reason);
      } else if (e.event === "contact_updated" || e.event === "contact_card_received") {
        // A contact was added/updated out-of-band — e.g. an inbound first-contact
        // Welcome made us join a group. Refresh the list so it appears without a
        // manual restart.
        void refreshContacts();
      }
    });
  }

  onMount(() => {
    // Guard: only subscribe when the daemon is actually running. If the first
    // onMount determined that the daemon isn't running and redirected to
    // /first-run, daemonConfirmed stays false and we skip the subscription
    // entirely, avoiding an unhandled rejection from a non-ready daemon.
    if (!daemonConfirmed) return;

    // Guard against mount-race: if the component unmounts before the async
    // reSubscribe promise resolves, tear down the just-created subscription
    // immediately rather than leaking it.
    let cancelled = false;
    reSubscribe().then(() => {
      if (cancelled && unsub) { unsub(); unsub = null; }
    }).catch(() => {
      // Rejection here means the daemon became unavailable between the
      // daemon_running check and subscribe; treat it as stream-closed so
      // the reconnect path handles it.
      handleStreamClosed(reSubscribe);
    });
    return () => {
      cancelled = true;
      if (unsub) { unsub(); unsub = null; }
    };
  });

  // Watch for IPC stream-closed events (emitted by the Tauri relay) and
  // self-heal with exponential backoff.
  onMount(() => {
    if (!daemonConfirmed) return;
    const unlistenP = listen<string>("ipc:stream-closed", () => {
      handleStreamClosed(reSubscribe);
    });
    return () => {
      unlistenP.then((unlisten) => unlisten());
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
        active={pubkeyEq($conversation.contact, c.pubkey)}
        expanded={pubkeyEq($expandedPubkey, c.pubkey)}
        onclick={() => selectContact(c)}
        onToggleExpanded={() => toggleExpanded(c.pubkey)}
      />
      {#if pubkeyEq($expandedPubkey, c.pubkey)}
        <ContactDetailsPanel summary={c} />
      {/if}
    {/each}
  </aside>
  <main class="pane">
    <header>
      <span class="title">{
        $contacts.find((c) => pubkeyEq(c.pubkey, $conversation.contact))?.nickname
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

{#if $connection.state !== "live"}
  <div class="reconnect-banner" role="status" aria-live="polite">
    {#if $connection.state === "reconnecting"}
      Reconnecting to the app service…
    {:else}
      Disconnected — <button type="button" class="retry-btn" onclick={() => handleStreamClosed(reSubscribe)}>retry</button>
    {/if}
  </div>
{/if}

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
  .reconnect-banner {
    position: fixed;
    top: var(--s-3);
    left: 50%;
    transform: translateX(-50%);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--fg-dim);
    border-radius: 6px;
    font: var(--t-ui);
    box-shadow: 0 2px 12px rgba(0, 0, 0, 0.4);
    z-index: 1000;
  }
  .retry-btn {
    background: none;
    border: none;
    color: var(--accent);
    font: var(--t-ui);
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
  }
</style>
