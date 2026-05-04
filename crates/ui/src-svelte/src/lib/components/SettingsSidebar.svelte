<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';

  const sections: Array<{ id: string; label: string }> = [
    { id: 'identity',      label: 'Identity'     },
    { id: 'mailboxes',     label: 'Mailboxes'    },
    { id: 'history',       label: 'History'      },
    { id: 'notifications', label: 'Notifications'},
    { id: 'advanced',      label: 'Advanced'     },
  ];

  $: pathSegments = $page.url.pathname.split('/').filter(Boolean);
  $: activeId = pathSegments.at(-1) ?? 'identity';
</script>

<nav class="sidebar" aria-label="Settings sections">
  <h2>Settings</h2>
  {#each sections as s}
    <button
      type="button"
      class:active={activeId === s.id}
      on:click={() => goto(`/settings/${s.id}`)}
    >{s.label}</button>
  {/each}
</nav>

<style>
  .sidebar {
    width: 200px;
    border-right: 1px solid var(--border, #2a2a2a);
    padding: var(--s-2, 8px);
    background: var(--bg-elevated, #16181d);
    display: flex;
    flex-direction: column;
    gap: var(--s-1, 4px);
    height: 100%;
    box-sizing: border-box;
  }
  h2 {
    font: var(--t-display, 20px / 1.3 "Inter", system-ui, sans-serif);
    margin: 0 0 var(--s-2, 8px) 0;
    color: var(--text, #e8eaed);
  }
  button {
    text-align: left;
    background: none;
    border: none;
    color: var(--text-muted, #9aa0a6);
    padding: var(--s-1, 4px) var(--s-2, 8px);
    border-radius: 4px;
    cursor: pointer;
    font: var(--t-ui, 13px / 1.4 "Inter", system-ui, sans-serif);
  }
  button.active { color: var(--text, #e8eaed); background: var(--bg, #0e0f12); }
  button:hover  { color: var(--text, #e8eaed); }
</style>
