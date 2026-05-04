<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { config, fetchConfig, patchConfig } from '$lib/stores/config';
  import type { NotificationMode } from '$lib/ipc/types';
  import { toast } from '$lib/stores/toast';

  const MODES: { id: NotificationMode; label: string; hint: string }[] = [
    { id: 'full',    label: 'Full',    hint: 'Sender + message preview' },
    { id: 'minimal', label: 'Minimal', hint: 'Sender only' },
    { id: 'generic', label: 'Generic', hint: '"New message"' },
    { id: 'off',     label: 'Off',     hint: 'No notifications' },
  ];

  onMount(() => { void fetchConfig(); });

  let snapshot = $derived($config.snapshot);

  async function setMode(mode: NotificationMode) {
    try {
      // ConfigPatch requires all fields; pass null for fields we're not changing.
      await patchConfig({
        notification_mode: mode,
        history_retention_days: null,
        direct_timeout_secs: null,
        close_to_tray: null,
        start_minimised: null,
        persist_logs_to_disk: null,
      });
      toast.show('Notification mode updated');
    } catch (e) {
      toast.show(`Failed: ${e}`);
    }
  }
</script>

<h1>Notifications</h1>

<fieldset>
  <legend>Mode</legend>
  {#each MODES as m}
    <label class="mode-row">
      <input
        type="radio"
        name="mode"
        value={m.id}
        checked={snapshot?.notification_mode === m.id}
        onchange={() => setMode(m.id)}
      />
      <strong>{m.label}</strong>
      <span class="hint">— {m.hint}</span>
    </label>
  {/each}
</fieldset>

<fieldset>
  <legend>Behaviour</legend>
  <label class="locked">
    <input type="checkbox" checked disabled />
    Suppress when window is focused AND the active conversation receives the message.
    Other conversations always notify.
  </label>
  <p class="muted">
    This behaviour is locked in Phase 2.F to keep the notification UX predictable.
  </p>
</fieldset>

<p class="muted">Per-conversation mute is on each contact's details panel.</p>

<style>
  fieldset {
    border: 1px solid var(--border, #2a2a2a);
    border-radius: 6px;
    padding: var(--s-2, 12px);
    margin-bottom: var(--s-3, 16px);
  }
  legend { padding: 0 var(--s-1, 6px); color: var(--text-muted, #888); }
  .mode-row { display: block; padding: var(--s-1, 6px) 0; }
  .hint { color: var(--text-muted, #888); }
  .locked { display: block; color: var(--text-muted, #888); }
  .muted { color: var(--text-muted, #888); font-size: 0.9em; }
</style>
