<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
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
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import { contacts, refreshContacts, expandedPubkey, toggleExpanded, pendingState, dropContact, removeContact } from "$lib/stores/contacts";
  import { now } from "$lib/stores/now";
  import { conversation, openConversationFromSummary, appendMessage } from "$lib/stores/conversation";
  import { torStatus } from "$lib/stores/tor_status";
  import { recordDeliveryStatus, hex16ToString } from "$lib/stores/delivery";
  import { applyProgress, applyReceived, applyFailed } from "$lib/stores/attachments";
  import { deepLinkInviteUrl } from "$lib/stores/deepLink";
  import { ipcClient } from "$lib/ipc/tauri";
  import { connection, handleStreamClosed } from "$lib/stores/connection";
  import { pubkeyEq } from "$lib/pubkey";
  import { toast } from "$lib/stores/toast";
  import { listen } from "@tauri-apps/api/event";
  import type { ContactSummary, PublicKey } from "$lib/ipc/types";

  let inviteOpen = $state(false);
  let addOpen = $state(false);
  let removeConfirmOpen = $state(false);
  // URL pre-filled when the dialog is opened via a skattr:// deep-link.
  let addInitialUrl = $state("");
  // Subscription teardown bookkeeping. The live subscription is started from
  // the async onMount below (only once the daemon is confirmed running), so a
  // `destroyed` flag guards against the component unmounting mid-await.
  let destroyed = false;
  let unlistenStreamClosed: (() => void) | null = null;

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
    await refreshContacts();
    // Daemon confirmed running: start the live event subscription + the
    // stream-closed self-heal listener. Starting them HERE (not in a separate
    // onMount that races an async-set flag) guarantees they run only on the
    // main-shell path and never when redirecting to /first-run.
    if (!destroyed) startMainShellSubscriptions();
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
          ? (pendingState(activeSummary, $now) === "failed"
              ? "Couldn't connect to this contact. Remove it and send a new invite to try again."
              : pendingState(activeSummary, $now) === "unconfirmed"
                ? "They haven't accepted your invite yet — you can't message them until they connect."
                : "Connecting… waiting for them to accept your invite.")
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
    // Guard at the source: a reconnect retry queued in handleStreamClosed's
    // backoff can fire after the page unmounts. Bailing here makes every caller
    // (the catch handler, the stream-closed listener, the retry button, and any
    // queued backoff retry) a no-op once destroyed, so no off-screen IPC wiring
    // is recreated after teardown.
    if (destroyed) return;
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
      } else if (e.event === "contact_removed") {
        // A contact was removed (pending wipe or archive). Drop the row; if it
        // was the open conversation, close it.
        dropContact(e.data);
        if (pubkeyEq($conversation.contact, e.data)) {
          conversation.update((s) => ({ ...s, contact: null, messages: [] }));
        }
      }
    });
  }

  // Start the main-shell subscriptions once the daemon is confirmed running.
  // Invoked from the async onMount above (not a separate onMount), so it never
  // races an async-set flag and never fires on the /first-run redirect path.
  // Self-heals: a subscribe rejection or an IPC stream-closed event both route
  // through the bounded-backoff reconnect path.
  function startMainShellSubscriptions(): void {
    reSubscribe()
      .then(() => {
        if (destroyed && unsub) { unsub(); unsub = null; }
      })
      .catch(() => {
        // Daemon became unavailable between the running check and subscribe;
        // treat it as stream-closed so the reconnect path handles it.
        handleStreamClosed(reSubscribe);
      });
    listen<string>("ipc:stream-closed", () => {
      handleStreamClosed(reSubscribe);
    }).then((unlisten) => {
      if (destroyed) { unlisten(); } else { unlistenStreamClosed = unlisten; }
    });
  }

  onDestroy(() => {
    destroyed = true;
    if (unsub) { unsub(); unsub = null; }
    if (unlistenStreamClosed) { unlistenStreamClosed(); unlistenStreamClosed = null; }
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
      {#if activeSummary?.group_state === "pending_join"}
        <div class="pending-actions">
          <button
            type="button"
            class="remove-btn"
            onclick={() => (removeConfirmOpen = true)}
          >
            Remove
          </button>
        </div>
      {/if}
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

{#if removeConfirmOpen && activeSummary?.group_state === "pending_join"}
  <ConfirmDialog
    title="Remove pending contact?"
    body="This clears the local invite attempt so you can start over. The contact will be removed immediately."
    confirmLabel="Remove"
    danger={true}
    onConfirm={async () => {
      if (!activeSummary) return;
      removeConfirmOpen = false;
      try {
        await removeContact(activeSummary.pubkey);
      } catch {
        toast.show("Failed to remove contact");
      }
    }}
    onCancel={() => (removeConfirmOpen = false)}
  />
{/if}

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
  .pending-actions {
    display: flex;
    justify-content: flex-end;
    padding: var(--s-1) var(--s-3);
    border-top: 1px solid var(--bg-elevated);
  }
  .remove-btn {
    padding: 6px var(--s-2);
    background: var(--danger);
    color: var(--text);
    border: none;
    border-radius: 4px;
    font: var(--t-ui);
    cursor: pointer;
  }
</style>
