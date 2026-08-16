<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";
  import { pendingState } from "$lib/stores/contacts";
  import { now } from "$lib/stores/now";

  let { summary, active = false, expanded = false, onclick, onToggleExpanded }: {
    summary: ContactSummary;
    active?: boolean;
    expanded?: boolean;
    onclick?: () => void;
    onToggleExpanded?: () => void;
  } = $props();

  // #101: an unconfirmed first contact reads as "Connecting…" during a grace
  // window, then "Not connected yet" once it's been pending too long. Re-derives
  // as the `now` store ticks so a stuck contact escalates without a daemon event.
  let pstate = $derived(pendingState(summary, $now));

  function shortHash(pk: string): string {
    return pk.length > 8 ? pk.slice(0, 8) : pk;
  }

  // #173: "connected" means the MLS group is established and this conversation
  // is usable — NOT that the peer is reachable right now. No presence signal
  // exists in ContactSummary today.
  //
  // Tested against "active" rather than `pstate === null`: pendingState() only
  // reports on `pending_join`, so its null case also covers `corrupt` and a
  // missing group_state, which must not read as connected.
  let connected = $derived(summary.group_state === "active");

  // #173: something unread, with no count. "7 unread" is itself a weak activity
  // signal; the row only needs to say "there is something new here".
  let hasUnread = $derived(summary.unread_count > 0);
</script>

<div class="row-wrap">
  <button class="row" class:active class:pending={pstate !== null} onclick={onclick}>
    <div class="title" class:connected>
      {summary.nickname ?? shortHash(summary.pubkey)}
      {#if pstate === "connecting"}
        <span class="pending-badge" title="First contact still connecting">Connecting…</span>
      {:else if pstate === "unconfirmed"}
        <span
          class="pending-badge unconfirmed"
          title="They haven't accepted your invite yet — still trying to reach them"
        >Not connected yet</span>
      {:else if pstate === "failed"}
        <span
          class="pending-badge failed"
          title="Couldn't connect — remove and send a new invite to try again"
        >Couldn't connect</span>
      {/if}
      {#if summary.muted}
        <span class="mute-icon" title="Muted" aria-label="Muted">🔕</span>
      {/if}
    </div>
    <!-- #173: the message preview and relative timestamp that used to live here
         are gone. The sidebar is persistent chrome — visible on every
         conversation, for every contact at once — so it showed the last thing
         anyone said to you continuously, plus an at-a-glance activity profile
         of every peer. Neither is needed to say "this has something new".

         The dot's slot is always rendered and toggled with `visibility` rather
         than `{#if}`: the row is a two-column grid, so conditionally removing
         it would collapse the column and shift the row. `visibility: hidden`
         also drops it from the accessibility tree, so an all-read row still
         exposes no unread affordance. -->
    <span
      class="unread-dot"
      class:on={hasUnread}
      aria-label={hasUnread ? "Unread messages" : undefined}
    ></span>
  </button>
  <button
    type="button"
    class="chevron"
    class:open={expanded}
    aria-label="Contact details"
    onclick={onToggleExpanded}
  >
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <polyline points="9 18 15 12 9 6"/>
    </svg>
  </button>
</div>

<style>
  .row-wrap {
    display: flex;
    align-items: stretch;
  }
  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--s-1);
    padding: var(--s-2) var(--s-3);
    background: transparent;
    border: none;
    text-align: left;
    color: var(--text);
    font: var(--t-body);
    cursor: pointer;
    flex: 1;
  }
  .row:hover, .row.active { background: var(--bg-elevated); }
  .title { font-weight: 500; display: flex; align-items: center; gap: 4px; }
  /* #173: an established (MLS-active) contact reads as connected. Mutually
     exclusive with `.row.pending` by construction — pendingState() only fires
     on `pending_join`, and this only on `active`. */
  .title.connected { font-weight: 700; color: var(--success, #2e9e4f); }
  .mute-icon { color: var(--text-muted, #888); font-size: 0.85em; }
  /* #101: a pending (unconfirmed) first contact is de-emphasised so it never
     reads as a normal, successfully-added contact. */
  .row.pending { opacity: 0.6; }
  .pending-badge { color: var(--text-muted, #888); font-size: 0.75em; font-weight: 400; }
  .pending-badge.unconfirmed { color: var(--warning, #c90); }
  .pending-badge.failed { color: var(--danger, #c33); }
  /* #173: fixed geometry so the row does not shift as unread state changes. */
  .unread-dot {
    width: 8px;
    height: 8px;
    border-radius: 999px;
    background: var(--accent);
    align-self: center;
    visibility: hidden;
  }
  .unread-dot.on { visibility: visible; }
  .chevron {
    background: transparent;
    border: none;
    padding: 0 var(--s-2);
    cursor: pointer;
    color: var(--text-muted);
    display: flex;
    align-items: center;
    transition: transform 0.15s ease;
  }
  .chevron:hover { color: var(--text); }
  .chevron.open svg { transform: rotate(90deg); }
</style>
