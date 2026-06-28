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

  const MIN_LENGTH = 8;

  let {
    mode = "create",
    onNext,
    onReset,
  }: {
    mode?: "create" | "unlock";
    onNext: (mnemonic?: string) => void;
    onReset?: () => void | Promise<void>;
  } = $props();

  let pass = $state("");
  let confirm = $state("");
  let strength = $state<number>(0);
  let feedback = $state<{ warning: string | null; suggestions: string[] } | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let confirmingReset = $state(false);

  async function evaluate() {
    if (!pass) {
      strength = 0;
      feedback = null;
      return;
    }
    const r = await zxcvbnAsync(pass);
    strength = r.score;
    feedback = r.feedback;
  }

  /** Build an actionable message explaining why a create-mode passphrase was rejected. */
  function weakPassphraseMessage(): string {
    const tips: string[] = [];
    if (feedback?.warning) tips.push(feedback.warning);
    if (feedback?.suggestions?.length) tips.push(...feedback.suggestions);
    return tips.length
      ? `Passphrase too weak. ${tips.join(" ")}`
      : "Passphrase too weak — combine several unrelated words, or make it longer.";
  }

  async function submit() {
    error = null;
    busy = true;
    try {
      if (mode === "create") {
        if (pass.length < MIN_LENGTH) {
          error = `Passphrase is too short — use at least ${MIN_LENGTH} characters.`;
          return;
        }
        if (pass !== confirm) {
          error = "Passphrases don't match.";
          return;
        }
        // Re-evaluate synchronously at submit time to avoid a stale `strength`
        // score from a previous async `evaluate()` call (race: user switches from
        // a strong to a weak passphrase and submits before the new score resolves).
        const zResult = await zxcvbnAsync(pass);
        strength = zResult.score;
        feedback = zResult.feedback;
        if (strength < 3) {
          error = weakPassphraseMessage();
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

  /** "Start a new identity" — wipe local data and restart onboarding. */
  async function doReset() {
    busy = true;
    error = null;
    try {
      await onReset?.();
    } catch (e) {
      error = String(e);
      confirmingReset = false;
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
    onkeydown={(e) => e.key === "Enter" && !busy && submit()}
    autocomplete="new-password"
  />
  {#if mode === "create"}
    <input
      type="password"
      placeholder="Confirm"
      bind:value={confirm}
      onkeydown={(e) => e.key === "Enter" && !busy && submit()}
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

  {#if mode === "unlock" && onReset}
    {#if !confirmingReset}
      <button class="link" disabled={busy} onclick={() => (confirmingReset = true)}>
        Start a new identity instead
      </button>
    {:else}
      <div class="reset-confirm">
        <p class="warn">
          This permanently deletes the identity, seed, and message history stored
          on this device and starts a fresh setup. It cannot be undone — back up
          your seed phrase first if you might need this identity again.
        </p>
        <button class="danger" disabled={busy} onclick={doReset}>
          Delete everything &amp; start new
        </button>
        <button class="link" disabled={busy} onclick={() => (confirmingReset = false)}>
          Cancel
        </button>
      </div>
    {/if}
  {/if}
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
  button.link {
    background: none;
    color: var(--text-muted);
    text-decoration: underline;
    padding: var(--s-1) 0;
    margin-top: var(--s-2);
    font-size: 0.9em;
  }
  button.danger {
    background: var(--danger);
    color: var(--bg);
  }
  .reset-confirm {
    margin-top: var(--s-3);
    padding-top: var(--s-3);
    border-top: 1px solid var(--text-muted);
  }
  .reset-confirm .warn {
    color: var(--danger);
    font-size: 0.9em;
    margin: 0 0 var(--s-2) 0;
  }
</style>
