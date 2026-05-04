<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import "$lib/tokens.css";
  import SearchPalette from "$lib/components/SearchPalette.svelte";
  import { openPalette } from "$lib/stores/searchPalette";
  import { deepLinkInviteUrl } from "$lib/stores/deepLink";

  let { children } = $props();

  function onGlobalKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      openPalette();
    }
  }

  // Forward skattr://invite/v1#… deep-link CustomEvents (dispatched by the
  // Rust host via wv.eval) into the deepLinkInviteUrl store so +page.svelte
  // can open the AddContactDialog with the URL pre-filled.
  let deepLinkHandler: ((e: Event) => void) | null = null;

  onMount(() => {
    window.addEventListener("keydown", onGlobalKey);
    deepLinkHandler = (e: Event) => {
      const url = (e as CustomEvent<string>).detail;
      if (typeof url === "string" && url.startsWith("skattr://invite/v1")) {
        deepLinkInviteUrl.set(url);
      }
    };
    window.addEventListener("skattr:deep-link", deepLinkHandler);
  });

  onDestroy(() => {
    window.removeEventListener("keydown", onGlobalKey);
    if (deepLinkHandler) {
      window.removeEventListener("skattr:deep-link", deepLinkHandler);
    }
  });
</script>

{@render children()}
<SearchPalette />
