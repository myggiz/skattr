<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  let {
    mnemonic,
    onNext,
  }: {
    mnemonic: string | null;
    onNext: () => void;
  } = $props();

  let revealed = $state(false);
  let typeBack = $state("");
  let skipModal = $state(false);
  let error = $state<string | null>(null);

  function normalise(s: string): string[] {
    return s.toLowerCase().split(/\s+/).filter(Boolean);
  }

  function confirmWords() {
    if (!mnemonic) {
      error = "no mnemonic";
      return;
    }
    const expected = normalise(mnemonic);
    const got = normalise(typeBack);
    if (
      expected.length !== got.length ||
      expected.some((w, i) => w !== got[i])
    ) {
      error = `Confirmation failed (expected ${expected.length} words in order).`;
      return;
    }
    onNext();
  }
</script>

<section class="step">
  <h1>Save your seed phrase</h1>
  <p class="warn">
    These 24 words are the only way to restore your identity. Skattr cannot
    recover them. Write them down somewhere safe before continuing.
  </p>
  {#if !revealed}
    <button onclick={() => (revealed = true)}>Reveal seed phrase</button>
  {:else if mnemonic}
    <pre class="seed">{mnemonic}</pre>
    <p>Type your seed phrase back to confirm.</p>
    <textarea
      bind:value={typeBack}
      placeholder="word1 word2 word3 …"
      rows="4"
    ></textarea>
    {#if error}<p class="error">{error}</p>{/if}
    <button onclick={confirmWords}>Confirm</button>
    <button class="link" onclick={() => (skipModal = true)}>
      I've written it down — skip type-back
    </button>
  {/if}
  {#if skipModal}
    <div class="modal">
      <div class="modal-body">
        <h2>Are you sure?</h2>
        <p class="warn">
          You will not be able to verify the seed phrase you wrote down.
          If you lose it, your identity is unrecoverable. Skattr will not
          ask again.
        </p>
        <button onclick={onNext}>Yes, skip confirmation</button>
        <button onclick={() => (skipModal = false)}>Cancel</button>
      </div>
    </div>
  {/if}
</section>

<style>
  .step { max-width: 60ch; margin: var(--s-4) auto; padding: var(--s-3); }
  .warn { color: var(--danger); }
  .seed {
    background: var(--bg-elevated);
    padding: var(--s-3);
    border-radius: 6px;
    user-select: all;
    word-spacing: var(--s-1);
    font: var(--t-body);
  }
  textarea {
    width: 100%;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--text-muted);
    border-radius: 4px;
    padding: var(--s-2);
    font: var(--t-body);
  }
  .error { color: var(--danger); }
  button {
    background: var(--accent);
    color: var(--bg);
    padding: var(--s-2) var(--s-3);
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font: var(--t-body);
    margin: var(--s-2) var(--s-2) 0 0;
  }
  .link { background: transparent; color: var(--text-muted); padding: 0; }
  .modal {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: grid;
    place-items: center;
    z-index: 100;
  }
  .modal-body {
    background: var(--bg-elevated);
    padding: var(--s-3);
    border: 2px solid var(--danger);
    border-radius: 8px;
    max-width: 50ch;
  }
</style>
