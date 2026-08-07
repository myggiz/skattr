<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";
  import { getPassphraseAuditLatest } from "$lib/stores/config";
  import { toast } from "$lib/stores/toast";
  import { copyText } from "$lib/clipboard";
  import ChangePassphraseDialog from "$lib/components/ChangePassphraseDialog.svelte";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";

  let pubkey = $state("");
  let onion = $state<string | null>(null);
  let lastChanged = $state<bigint | null>(null);
  let showPassphrase = $state(false);
  let confirmRotate = $state(false);

  onMount(async () => {
    try {
      const resp = await ipcClient.request({ cmd: "daemon_info" });
      const result = unwrapOk(resp);
      if (result.result !== "daemon_info") {
        throw new Error(`unexpected reply: ${result.result}`);
      }
      pubkey = result.data.local_pubkey;
      onion = result.data.current_onion;
    } catch (e) {
      toast.show(`Failed to load identity: ${e}`);
    }
    try {
      lastChanged = await getPassphraseAuditLatest();
    } catch {
      // Non-fatal — leave as null ("Never").
    }
  });

  async function copy(s: string) {
    try {
      await copyText(s);
      toast.show("Copied");
    } catch (e) {
      toast.show(`Copy failed: ${e}`);
    }
  }

  async function rotateOnion() {
    confirmRotate = false;
    try {
      const resp = await ipcClient.request({ cmd: "rotate_onion" });
      if (resp.resp !== "ok") {
        throw new Error(`rotate_onion failed: ${JSON.stringify(resp)}`);
      }
      toast.show("Onion rotation queued");
    } catch (e) {
      toast.show(`Rotate failed: ${e}`);
    }
  }

  function fmtTs(t: bigint | null): string {
    return t ? new Date(Number(t) * 1000).toLocaleString() : "Never";
  }
</script>

<h1>Identity</h1>

<dl>
  <dt>Public key</dt>
  <dd>
    <code>{pubkey ? pubkey.slice(0, 16) + "…" : "(loading)"}</code>
    {#if pubkey}
      <button type="button" onclick={() => copy(pubkey)}>Copy full</button>
    {/if}
  </dd>

  <dt>Onion</dt>
  <dd>
    <code>{onion ? onion.slice(0, 16) + "…" : "(loading)"}</code>
    {#if onion}
      <button type="button" onclick={() => copy(onion!)}>Copy full</button>
    {/if}
  </dd>

  <dt>Passphrase last changed</dt>
  <dd>{fmtTs(lastChanged)}</dd>
</dl>

<div class="actions">
  <button type="button" onclick={() => (confirmRotate = true)}>Rotate onion</button>
  <button type="button" onclick={() => (showPassphrase = true)}>Change passphrase</button>
</div>

{#if confirmRotate}
  <ConfirmDialog
    title="Rotate onion?"
    body="In Phase 2.F this only bumps the self-card version (real HS-key rotation lands in a follow-up)."
    confirmLabel="Rotate"
    onConfirm={rotateOnion}
    onCancel={() => (confirmRotate = false)}
  />
{/if}

{#if showPassphrase}
  <ChangePassphraseDialog onClose={() => (showPassphrase = false)} />
{/if}

<style>
  h1 { font: var(--t-display); margin: 0 0 var(--s-3); }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--s-1, 6px) var(--s-3, 16px);
    margin-bottom: var(--s-3, 16px);
  }
  dt { color: var(--text-muted, #888); font: var(--t-ui); align-self: center; }
  dd { margin: 0; display: flex; align-items: center; gap: var(--s-2, 12px); }
  code {
    font-family: monospace;
    background: var(--bg, #111);
    padding: 2px 6px;
    border-radius: 3px;
  }
  .actions { display: flex; gap: var(--s-2, 12px); margin-top: var(--s-3, 16px); }
  button { padding: 8px 16px; cursor: pointer; }
</style>
