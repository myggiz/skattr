<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { icons } from "$lib/icons";
  import { theme, setTheme, nextTheme } from "$lib/stores/theme";

  const GLYPH = { system: "monitor", light: "sun", dark: "moon" } as const;
  const NAME = { system: "Match system", light: "Light", dark: "Dark" } as const;
</script>

<button
  type="button"
  class="toggle"
  onclick={() => setTheme(nextTheme($theme))}
  title="Theme: {NAME[$theme]} — click for {NAME[nextTheme($theme)]}"
  aria-label="Theme: {NAME[$theme]}. Switch to {NAME[nextTheme($theme)]}."
>
  {@html icons[GLYPH[$theme]]}
</button>

<style>
  .toggle {
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
  .toggle:hover { color: var(--text); border-color: var(--hairline); }
  .toggle :global(svg) { width: 17px; height: 17px; }
</style>
