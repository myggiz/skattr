<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import "$lib/tokens.css";
  import { applyTheme, theme } from "$lib/stores/theme";
  import SearchPalette from "$lib/components/SearchPalette.svelte";
  import { openPalette } from "$lib/stores/searchPalette";
  import { deepLinkInviteUrl } from "$lib/stores/deepLink";

  let { children } = $props();

  // Applied at module scope rather than from an inline <script> in app.html:
  // the CSP in tauri.conf.json is `script-src 'self'` with no 'unsafe-inline',
  // and BOTH that policy and the meta one in app.html must admit a resource
  // (#172, #215). Weakening the app-wide script policy to avoid one frame of
  // the wrong background is not a trade worth making, so the stamp happens as
  // early as the bundle can run instead.
  if (typeof document !== "undefined") {
    applyTheme(document.documentElement, $theme);
  }

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
