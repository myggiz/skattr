<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { zxcvbnAsync, zxcvbnOptions } from "@zxcvbn-ts/core";
  import { dictionary, translations } from "@zxcvbn-ts/language-en";

  // Must call setOptions once before first zxcvbnAsync call.
  zxcvbnOptions.setOptions({
    translations,
    dictionary,
  });

  let {
    mode = "create",
    onNext,
  }: {
    mode?: "create" | "unlock";
    onNext: (mnemonic?: string) => void;
  } = $props();

  let pass = $state("");
  let confirm = $state("");
  let strength = $state<number>(0);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function evaluate() {
    if (!pass) {
      strength = 0;
      return;
    }
    const r = await zxcvbnAsync(pass);
    strength = r.score;
  }

  async function submit() {
    error = null;
    busy = true;
    try {
      if (mode === "create") {
        if (pass !== confirm) {
          error = "Passphrases don't match.";
          return;
        }
        if (strength < 3) {
          error = "Passphrase too weak (need at least 3/4).";
          return;
        }
        const r = await invoke<{ mnemonic: string }>("identity_init", {
          args: { passphrase: pass, mnemonic: null },
        });
        await invoke("vault_unlock", { args: { passphrase: pass } });
        onNext(r.mnemonic);
      } else {
        await invoke("vault_unlock", { args: { passphrase: pass } });
        onNext();
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="step">
  <h1>{mode === "create" ? "Create a passphrase" : "Unlock"}</h1>
  <input
    type="password"
    placeholder="Passphrase"
    bind:value={pass}
    oninput={evaluate}
    autocomplete="new-password"
  />
  {#if mode === "create"}
    <input
      type="password"
      placeholder="Confirm"
      bind:value={confirm}
      autocomplete="new-password"
    />
    <div class="meter" data-strength={strength}>
      <span></span><span></span><span></span><span></span>
    </div>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  <button disabled={busy} onclick={submit}>
    {mode === "create" ? "Create identity" : "Unlock"}
  </button>
</section>

<style>
  .step { max-width: 40ch; margin: var(--s-4) auto; padding: var(--s-3); }
  input {
    display: block;
    width: 100%;
    margin: var(--s-2) 0;
    padding: var(--s-2);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--text-muted);
    border-radius: 4px;
    font: var(--t-body);
  }
  .meter { display: flex; gap: 4px; margin-top: var(--s-2); }
  .meter span { flex: 1; height: 4px; background: var(--text-muted); border-radius: 2px; }
  .meter[data-strength="1"] span:nth-child(-n+1),
  .meter[data-strength="2"] span:nth-child(-n+2),
  .meter[data-strength="3"] span:nth-child(-n+3),
  .meter[data-strength="4"] span:nth-child(-n+4) { background: var(--accent); }
  .error { color: var(--danger); }
  button {
    background: var(--accent);
    color: var(--bg);
    padding: var(--s-2) var(--s-3);
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font: var(--t-body);
    margin-top: var(--s-3);
  }
  button:disabled { opacity: 0.5; cursor: progress; }
</style>
