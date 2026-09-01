<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import { icons } from "$lib/icons";
  import { openPalette } from "$lib/stores/searchPalette";
  import ThemeToggle from "./ThemeToggle.svelte";
</script>

<!-- Whole-app controls, kept out of the contact list so the list is only
     contacts. Search was previously reachable by Ctrl+K alone. -->
<nav class="app-rail" aria-label="Application">
  <button type="button" onclick={openPalette} title="Search messages (Ctrl+K)" aria-label="Search messages">
    {@html icons["search"]}
  </button>

  <div class="spacer"></div>

  <ThemeToggle />
  <button type="button" onclick={() => goto("/settings/identity")} title="Settings" aria-label="Open settings">
    {@html icons["settings"]}
  </button>
</nav>

<style>
  .app-rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s-1);
    padding: var(--s-2) 0;
    width: 52px;
    background: var(--bg-sunk);
    border-right: 1px solid var(--hairline);
  }
  .spacer { flex: 1; }
  button {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    padding: 0;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    color: var(--text-muted);
    cursor: pointer;
  }
  button:hover { color: var(--text); border-color: var(--hairline); }
  button :global(svg) { width: 17px; height: 17px; }
</style>
