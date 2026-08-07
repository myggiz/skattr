<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  interface Props {
    title: string;
    body: string;
    confirmLabel: string;
    cancelLabel?: string;
    danger?: boolean;
    onConfirm: () => Promise<void> | void;
    onCancel: () => void;
  }

  let {
    title,
    body,
    confirmLabel,
    cancelLabel = "Cancel",
    danger = false,
    onConfirm,
    onCancel,
  }: Props = $props();

  let busy = $state(false);

  async function handleConfirm() {
    if (busy) return;
    busy = true;
    try {
      await onConfirm();
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="confirm-title">
  <div class="dialog">
    <h2 id="confirm-title">{title}</h2>
    <p>{body}</p>
    <div class="actions">
      <button type="button" onclick={onCancel} disabled={busy}>{cancelLabel}</button>
      <button
        type="button"
        class:danger
        onclick={handleConfirm}
        disabled={busy}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid;
    place-items: center;
    z-index: 900;
  }
  .dialog {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-3);
    border-radius: 8px;
    max-width: 480px;
    width: 90vw;
  }
  .dialog h2 { font: var(--t-display); margin: 0 0 var(--s-2); }
  .dialog p  { font: var(--t-body); margin: 0 0 var(--s-3); }
  .actions { display: flex; justify-content: flex-end; gap: var(--s-2); }
  button { padding: 8px 16px; cursor: pointer; }
  button.danger { background: var(--danger); color: var(--text); border: none; }
</style>
