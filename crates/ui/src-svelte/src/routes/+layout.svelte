<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import "$lib/tokens.css";
  import SearchPalette from "$lib/components/SearchPalette.svelte";
  import { openPalette } from "$lib/stores/searchPalette";

  let { children } = $props();

  function onGlobalKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      openPalette();
    }
  }

  onMount(() => window.addEventListener("keydown", onGlobalKey));
  onDestroy(() => window.removeEventListener("keydown", onGlobalKey));
</script>

{@render children()}
<SearchPalette />
