<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { zxcvbnAsync, zxcvbnOptions } from "@zxcvbn-ts/core";
  import { dictionary, translations } from "@zxcvbn-ts/language-en";
  import { changePassphrase } from "$lib/stores/config";
  import { toast } from "$lib/stores/toast";

  zxcvbnOptions.setOptions({ translations, dictionary });

  interface Props {
    onClose: () => void;
  }
  let { onClose }: Props = $props();

  let oldPass = $state("");
  let newPass = $state("");
  let confirm = $state("");
  let inflight = $state(false);
  let errorMsg = $state("");
  let score = $state(0);

  const STRENGTH_LABEL = ["weakest", "weak", "fair", "good", "strong"];

  async function evaluateStrength() {
    if (!newPass) { score = 0; return; }
    const r = await zxcvbnAsync(newPass);
    score = r.score;
  }

  $effect(() => {
    evaluateStrength();
  });

  let canSubmit = $derived(
    !inflight
    && newPass.length >= 8
    && score >= 3
    && newPass === confirm
    && newPass !== oldPass
  );

  async function submit() {
    if (!canSubmit) return;
    inflight = true;
    errorMsg = "";
    try {
      await changePassphrase(oldPass, newPass);
      toast.show("Passphrase changed");
      onClose();
    } catch (e: unknown) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      inflight = false;
    }
  }

  function onOverlayKeydown(e: KeyboardEvent) {
    if (!inflight && e.key === "Escape") onClose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="modal-overlay"
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  onkeydown={onOverlayKeydown}
>
  <div class="modal">
    <h2>Change passphrase</h2>

    <label>
      Current passphrase
      <input
        type="password"
        bind:value={oldPass}
        disabled={inflight}
        autocomplete="current-password"
      />
    </label>

    <label>
      New passphrase
      <input
        type="password"
        bind:value={newPass}
        disabled={inflight}
        autocomplete="new-password"
      />
    </label>
    <div class="strength" data-score={score}>
      Strength: {STRENGTH_LABEL[score]}
    </div>

    <label>
      Confirm new
      <input
        type="password"
        bind:value={confirm}
        disabled={inflight}
        autocomplete="new-password"
      />
    </label>

    {#if errorMsg}
      <div class="error" role="alert">{errorMsg}</div>
    {/if}

    {#if inflight}
      <div class="spinner">Changing passphrase…</div>
    {/if}

    <div class="actions">
      <button type="button" onclick={onClose} disabled={inflight}>Cancel</button>
      <button type="button" onclick={submit} disabled={!canSubmit}>Change</button>
    </div>
  </div>
</div>

<style>
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--bg-elevated, #1a1a1a);
    color: var(--text, #ddd);
    padding: var(--s-3, 16px);
    border-radius: 8px;
    min-width: 360px;
    display: flex;
    flex-direction: column;
    gap: var(--s-2, 12px);
  }
  h2 { font: var(--t-display); margin: 0; }
  label {
    display: flex;
    flex-direction: column;
    gap: var(--s-1, 6px);
    font: var(--t-ui);
  }
  input {
    background: var(--bg, #111);
    color: var(--text, #ddd);
    border: 1px solid var(--hairline);
    padding: var(--s-1, 6px);
    border-radius: 4px;
    font: var(--t-body);
  }
  .strength { font-size: 0.85em; color: var(--text-muted, #888); }
  .strength[data-score="0"], .strength[data-score="1"] { color: var(--danger, #d33); }
  .strength[data-score="2"] { color: #d80; }
  .strength[data-score="3"], .strength[data-score="4"] { color: var(--accent, #6c6); }
  .error { color: var(--danger, #d33); }
  .spinner { color: var(--text-muted, #888); }
  .actions {
    display: flex;
    gap: var(--s-2, 12px);
    justify-content: flex-end;
    margin-top: var(--s-2, 12px);
  }
  button { padding: 8px 16px; cursor: pointer; }
</style>
