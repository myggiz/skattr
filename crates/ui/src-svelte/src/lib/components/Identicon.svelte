<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { GRID, identiconCells, identiconHue } from "$lib/identicon";

  let { pubkey, size = 34 }: { pubkey: string; size?: number } = $props();

  let cells = $derived(identiconCells(pubkey));
  let hue = $derived(identiconHue(pubkey));

  // Drawn in grid units and scaled by the SVG viewBox, so one set of numbers
  // works at every size.
  const PAD = 0.5;
  const SPAN = GRID + PAD * 2;
</script>

<!-- Decorative: the contact's name is always rendered next to it, and the
     fingerprint that actually matters lives in the details panel. -->
<svg
  class="identicon"
  width={size}
  height={size}
  viewBox="0 0 {SPAN} {SPAN}"
  style="--h: {hue}"
  aria-hidden="true"
>
  <rect class="tile" x="0" y="0" width={SPAN} height={SPAN} rx="1.4" />
  {#each cells as on, i}
    {#if on}
      <rect
        class="cell"
        x={PAD + (i % GRID)}
        y={PAD + Math.floor(i / GRID)}
        width="1"
        height="1"
        rx="0.28"
      />
    {/if}
  {/each}
</svg>

<style>
  .identicon {
    display: block;
    flex: none;
    border-radius: 8px;
    overflow: hidden;
  }
  .tile { fill: var(--bg-sunk); }
  /* Saturation and lightness are fixed so every figure is the same material
     and only the hue identifies the key; this mid-tone clears 3:1 on both
     the dark and the light ground. */
  .cell { fill: hsl(var(--h) 52% 55%); }
</style>
