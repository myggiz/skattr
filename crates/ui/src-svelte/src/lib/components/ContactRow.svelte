<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
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

  function relativeTs(ts: bigint | null | undefined): string {
    if (!ts) return "";
    const tsNum = Number(ts);
    const now = Math.floor(Date.now() / 1000);
    const delta = now - tsNum;
    if (delta < 60) return `${delta}s`;
    if (delta < 3600) return `${Math.floor(delta / 60)}m`;
    if (delta < 86400) return `${Math.floor(delta / 3600)}h`;
    return new Date(tsNum * 1000).toLocaleDateString();
  }
</script>

<div class="row-wrap">
  <button class="row" class:active class:pending={pstate !== null} onclick={onclick}>
    <div class="title">
      {summary.nickname ?? shortHash(summary.pubkey)}
      {#if pstate === "connecting"}
        <span class="pending-badge" title="First contact still connecting">Connecting…</span>
      {:else if pstate === "unconfirmed"}
        <span
          class="pending-badge unconfirmed"
          title="They haven't accepted your invite yet — still trying to reach them"
        >Not connected yet</span>
      {/if}
      {#if summary.muted}
        <span class="mute-icon" title="Muted" aria-label="Muted">🔕</span>
      {/if}
    </div>
    <div class="meta">
      <span class="preview">{summary.last_message_preview ?? ""}</span>
      <span class="ts">{relativeTs(summary.last_ts_recv)}</span>
    </div>
    {#if summary.unread_count > 0}
      <span class="badge">{summary.unread_count}</span>
    {/if}
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
  .mute-icon { color: var(--text-muted, #888); font-size: 0.85em; }
  /* #101: a pending (unconfirmed) first contact is de-emphasised so it never
     reads as a normal, successfully-added contact. */
  .row.pending { opacity: 0.6; }
  .pending-badge { color: var(--text-muted, #888); font-size: 0.75em; font-weight: 400; }
  .pending-badge.unconfirmed { color: var(--warning, #c90); }
  .meta { display: flex; justify-content: space-between; color: var(--text-muted); font: var(--t-ui); }
  .preview { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 200px; }
  .badge {
    background: var(--accent);
    color: var(--bg);
    border-radius: 999px;
    padding: 0 var(--s-1);
    font: var(--t-ui);
    align-self: center;
  }
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
