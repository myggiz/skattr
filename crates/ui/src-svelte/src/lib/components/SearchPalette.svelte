<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";
  import {
    searchPalette,
    closePalette,
    setQuery,
    setResults,
    setLoading,
    setFocusedRowId,
  } from "$lib/stores/searchPalette";
  import { contacts } from "$lib/stores/contacts";
  import { openConversationFromSummary } from "$lib/stores/conversation";
  import { get } from "svelte/store";

  let { inline = false }: { inline?: boolean } = $props();

  let queryStr = $state("");
  let timer: ReturnType<typeof setTimeout> | null = null;
  let highlightIdx = $state(0);

  // Build a pubkey→nickname lookup from the contacts store
  let nicknameMap = $derived(
    new Map($contacts.map((c) => [c.pubkey, c.nickname ?? null]))
  );

  /** Convert FTS5 snippet (STX/ETX markers) to safe HTML. */
  function snippetToHtml(raw: string): string {
    const STX = "\x02";
    const ETX = "\x03";
    const parts = raw.split(new RegExp(`(${STX}[^${ETX}]*${ETX})`, "g"));
    return parts
      .map((part) => {
        if (part.startsWith(STX) && part.endsWith(ETX)) {
          const inner = escHtml(part.slice(1, -1));
          return `<mark>${inner}</mark>`;
        }
        return escHtml(part);
      })
      .join("");
  }

  function escHtml(s: string): string {
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  async function runQuery(q: string) {
    if (!q.trim()) {
      setResults([]);
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const resp = await ipcClient.request({
        cmd: "search_messages",
        query: q,
        contact: null,
        limit: 50,
        offset: 0,
        newest_first: true,
      });
      const result = unwrapOk(resp);
      if (result.result === "search_results") {
        setResults(result.data);
        highlightIdx = 0;
      } else {
        setResults([]);
      }
    } catch (e) {
      setResults([]);
      console.error("search failed:", e);
    }
  }

  function onInput(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    queryStr = v;
    setQuery(v);
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => void runQuery(v), 200);
  }

  function pick(i: number) {
    const r = $searchPalette.results[i];
    if (!r) return;
    if (!inline) closePalette();
    // Open the conversation via the store rather than goto('/conversation/...')
    // because this is a SPA with no /conversation/[contact] route. We open the
    // contact's conversation then set focusedRowId so VirtualMessageList can
    // scroll the target row into view.
    const contactSummary = get(contacts).find((c) => c.pubkey === r.record.contact);
    if (contactSummary) {
      void openConversationFromSummary(contactSummary).then(() => {
        setFocusedRowId(r.record.row_id);
      });
    } else {
      // Contact not in list (edge case) — fall back to just setting focus.
      setFocusedRowId(r.record.row_id);
    }
  }

  function handleOverlayKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (!inline) closePalette();
    }
  }

  function onPanelKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !inline) {
      closePalette();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      highlightIdx = Math.min(
        highlightIdx + 1,
        $searchPalette.results.length - 1
      );
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      highlightIdx = Math.max(highlightIdx - 1, 0);
    } else if (e.key === "Enter") {
      pick(highlightIdx);
    }
  }

  // Reset local state when palette is closed externally
  $effect(() => {
    if (!$searchPalette.open && !inline) {
      queryStr = "";
      highlightIdx = 0;
    }
  });
</script>

{#if inline || $searchPalette.open}
  <div class="palette" class:modal={!inline} class:inline>
    {#if !inline}
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div
        class="overlay"
        role="button"
        aria-label="Close search"
        onclick={closePalette}
        onkeydown={handleOverlayKey}
      ></div>
    {/if}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- role="dialog" + onkeydown is the standard modal keyboard-trap pattern -->
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="panel"
      role="dialog"
      aria-modal={!inline}
      aria-label="Search messages"
      onkeydown={onPanelKey}
    >
      <!-- svelte-ignore a11y_autofocus -->
      <!-- autofocus is intentional: the palette opens on demand and focus must land in the search field immediately -->
      <input
        type="text"
        value={queryStr}
        oninput={onInput}
        placeholder="Search messages…"
        autofocus={!inline}
        aria-label="Search query"
      />
      {#if $searchPalette.loading}
        <div class="status">Searching…</div>
      {/if}
      <ul role="listbox" aria-label="Search results">
        {#each $searchPalette.results as r, i (r.record.row_id)}
          <!-- svelte-ignore a11y_interactive_supports_focus -->
          <li
            role="option"
            aria-selected={i === highlightIdx}
            class:active={i === highlightIdx}
            onclick={() => pick(i)}
            onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); pick(i); } }}
          >
            <div class="meta">
              {nicknameMap.get(r.record.contact) ?? r.record.contact.slice(0, 8) + "…"}
              · {new Date(Number(r.record.ts_daemon_recv) * 1000).toLocaleString()}
            </div>
            <div class="snippet">{@html snippetToHtml(r.snippet)}</div>
          </li>
        {/each}
        {#if !$searchPalette.loading && queryStr && $searchPalette.results.length === 0}
          <li class="empty" role="option" aria-selected={false}>No matches.</li>
        {/if}
      </ul>
    </div>
  </div>
{/if}

<style>
  .palette.modal {
    position: fixed;
    inset: 0;
    z-index: 200;
  }
  .overlay {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    cursor: default;
  }
  .panel {
    position: relative;
    max-width: 60vw;
    margin: 10vh auto;
    background: var(--bg-elevated, #1a1a1a);
    color: var(--text, #ddd);
    padding: var(--s-2, 12px);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }
  .palette.inline .panel {
    margin: 0;
    max-width: 100%;
    box-shadow: none;
  }
  input[type="text"] {
    width: 100%;
    background: var(--bg, #111);
    color: var(--text, #ddd);
    border: 1px solid var(--border, #2a2a2a);
    padding: var(--s-1, 6px) var(--s-2, 12px);
    border-radius: 4px;
    font: var(--t-body);
    box-sizing: border-box;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: var(--s-2, 12px) 0 0 0;
    max-height: 50vh;
    overflow-y: auto;
  }
  li {
    padding: var(--s-1, 6px) var(--s-2, 12px);
    cursor: pointer;
    border-radius: 4px;
  }
  li.active {
    background: var(--bg, #111);
  }
  li.empty {
    color: var(--text-muted, #888);
    cursor: default;
  }
  .meta {
    font-size: 0.85em;
    color: var(--text-muted, #888);
    margin-bottom: 4px;
  }
  .snippet {
    font-family: monospace;
    font-size: 0.9em;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .snippet :global(mark) {
    background: var(--accent, #5b5fc7);
    color: var(--text, #ddd);
    border-radius: 2px;
    padding: 0 2px;
  }
  .status {
    color: var(--text-muted, #888);
    padding: var(--s-2, 12px);
  }
</style>
