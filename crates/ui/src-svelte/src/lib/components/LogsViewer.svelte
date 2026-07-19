<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { logs, attachLogs, detachLogs } from "$lib/stores/logs";
  import { toast } from "$lib/stores/toast";
  import { copyText } from "$lib/clipboard";

  onMount(() => {
    void attachLogs();
  });
  onDestroy(() => {
    detachLogs();
  });

  async function copyAll() {
    const text = $logs
      .map(
        (r) =>
          `${new Date(Number(r.ts_unix_ms)).toISOString()} ${String(r.level).toUpperCase().padEnd(5)} ${r.target} ${r.message}`,
      )
      .join("\n");
    try {
      await copyText(text);
      toast.show("Logs copied");
    } catch (e) {
      toast.show(`Copy failed: ${e}`);
    }
  }
</script>

<div class="logs-viewer">
  <div class="header">
    <span class="count">{$logs.length} records</span>
    <button type="button" onclick={copyAll}>Copy all</button>
  </div>
  <ol>
    {#each $logs as r (r.seq)}
      <li class="lvl-{String(r.level).toLowerCase()}">
        <span class="ts">{new Date(Number(r.ts_unix_ms)).toLocaleTimeString()}</span>
        <span class="lvl">{r.level}</span>
        <span class="target">{r.target}</span>
        <span class="msg">{r.message}</span>
      </li>
    {/each}
    {#if $logs.length === 0}
      <li class="empty">No log records buffered yet.</li>
    {/if}
  </ol>
</div>

<style>
  .logs-viewer {
    font: var(--t-ui);
    max-height: 400px;
    overflow: auto;
    background: var(--bg);
    border: 1px solid var(--border, #2a2a2a);
    border-radius: 4px;
    margin: var(--s-2) 0;
  }
  .header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--s-1) var(--s-2);
    background: var(--bg-elevated);
    position: sticky;
    top: 0;
  }
  .count {
    color: var(--text-muted);
    font-size: 0.85em;
  }
  ol {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  li {
    padding: 2px var(--s-2);
    font-family: monospace;
    font-size: 0.82em;
    display: grid;
    grid-template-columns: 80px 52px 220px 1fr;
    gap: var(--s-1);
    border-bottom: 1px solid var(--border, #1a1a1a);
  }
  li:last-child {
    border-bottom: none;
  }
  .lvl-trace,
  .lvl-debug {
    color: var(--text-muted);
  }
  .lvl-info {
    color: var(--text);
  }
  .lvl-warn {
    color: #c8a000;
  }
  .lvl-error {
    color: var(--danger);
  }
  .empty {
    display: block;
    padding: var(--s-2);
    color: var(--text-muted);
    text-align: center;
    font-family: inherit;
    font-size: 0.9em;
  }
  .target {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
