<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";

  let { summary, active = false, expanded = false, onclick, onToggleExpanded }: {
    summary: ContactSummary;
    active?: boolean;
    expanded?: boolean;
    onclick?: () => void;
    onToggleExpanded?: () => void;
  } = $props();

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
  <button class="row" class:active onclick={onclick}>
    <div class="title">
      {summary.nickname ?? shortHash(summary.pubkey)}
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
