<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import SettingsSidebar from '$lib/components/SettingsSidebar.svelte';
  import Toast from '$lib/components/Toast.svelte';

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') goto('/');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });
</script>

<div class="settings-shell">
  <SettingsSidebar />
  <main class="content">
    <slot />
  </main>
</div>
<Toast />

<style>
  .settings-shell {
    display: grid;
    grid-template-columns: 200px 1fr;
    height: 100vh;
  }
  .content {
    padding: var(--s-3, 16px);
    overflow-y: auto;
  }
</style>
