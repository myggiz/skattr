<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { ContactSummary } from "$lib/ipc/types";

  let { summary, active = false, onclick }: {
    summary: ContactSummary;
    active?: boolean;
    onclick?: () => void;
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

<button class="row" class:active onclick={onclick}>
  <div class="title">
    {summary.nickname ?? shortHash(summary.pubkey)}
  </div>
  <div class="meta">
    <span class="preview">{summary.last_message_preview ?? ""}</span>
    <span class="ts">{relativeTs(summary.last_ts_recv)}</span>
  </div>
  {#if summary.unread_count > 0}
    <span class="badge">{summary.unread_count}</span>
  {/if}
</button>

<style>
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
    width: 100%;
  }
  .row:hover, .row.active { background: var(--bg-elevated); }
  .title { font-weight: 500; }
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
</style>
