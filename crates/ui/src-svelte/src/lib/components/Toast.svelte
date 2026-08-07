<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { currentToast } from "$lib/stores/toast";
</script>

{#if $currentToast}
  <div class="toast" role="status" aria-live="polite">
    {$currentToast.message}
  </div>
{/if}

<style>
  .toast {
    position: fixed;
    bottom: var(--s-3);
    right: var(--s-3);
    padding: var(--s-2) var(--s-3);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--bg-elevated);
    border-radius: 6px;
    font: var(--t-ui);
    box-shadow: 0 2px 12px rgba(0, 0, 0, 0.4);
    z-index: 1000;
  }
</style>
