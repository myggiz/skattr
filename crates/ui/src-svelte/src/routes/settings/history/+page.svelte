<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { config, fetchConfig, patchConfig } from '$lib/stores/config';
  import { ipcClient } from '$lib/ipc/tauri';
  import { unwrapOk } from '$lib/ipc/client';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { toast } from '$lib/stores/toast';
  import type { PublicKey } from '$lib/ipc/types';

  type ExportFormat = 'jsonl' | 'plaintext';
  let format: ExportFormat = $state('plaintext');
  let pruneDays: number = $state(30);
  let confirmPrune: { older_than_days: number } | null = $state(null);
  let exportInflight: boolean = $state(false);

  onMount(() => { void fetchConfig(); });

  const PRESETS: { label: string; days: number }[] = [
    { label: '24 hours',  days: 1 },
    { label: '7 days',    days: 7 },
    { label: '30 days',   days: 30 },
    { label: '90 days',   days: 90 },
    { label: 'Never delete', days: 0 },
  ];

  // Reactive snapshot derived from the config store.
  let snapshot = $derived($config.snapshot);

  async function setRetention(days: number) {
    try {
      await patchConfig({
        history_retention_days: days,
        direct_timeout_secs: null,
        notification_mode: null,
        close_to_tray: null,
        start_minimised: null,
        persist_logs_to_disk: null,
      });
      toast.show('Retention updated');
    } catch (e) {
      toast.show(`Failed: ${e}`);
    }
  }

  // Streaming export: fetch all contacts, page through ExportHistory per
  // contact, accumulate formatted lines, then copy to clipboard.
  // Note: ExportHistory requires a specific contact (PublicKey) — there is
  // no all-conversations variant in the current wire format. We iterate
  // contacts client-side. A Tauri save-dialog plugin is not bundled in
  // this release; clipboard copy is the pragmatic 2.F fallback.
  async function downloadAll() {
    exportInflight = true;
    try {
      // Fetch contact list.
      const contactsResp = unwrapOk(await ipcClient.request({ cmd: 'list_contacts' }));
      if (contactsResp.result !== 'contacts') {
        throw new Error(`unexpected reply: ${contactsResp.result}`);
      }
      const contacts = contactsResp.data;

      const lines: string[] = [];

      for (const c of contacts) {
        const contact: PublicKey = c.pubkey;
        let afterId: bigint | null = null;

        // eslint-disable-next-line no-constant-condition
        while (true) {
          const pageResp = unwrapOk(await ipcClient.request({
            cmd: 'export_history',
            contact,
            after_id: afterId,
            limit: 500,
          }));
          if (pageResp.result !== 'export_page') {
            throw new Error(`unexpected reply: ${pageResp.result}`);
          }
          const { records, next_after_id } = pageResp.data;
          if (records.length === 0) break;
          for (const r of records) {
            lines.push(format === 'jsonl' ? jsonlLine(r) : plaintextLine(r, c.nickname ?? null));
          }
          afterId = next_after_id;
          if (afterId === null || records.length < 500) break;
        }
      }

      if (lines.length === 0) {
        toast.show('No messages to export');
        return;
      }

      const blob = lines.join('');
      await navigator.clipboard.writeText(blob);
      toast.show(`Copied ${lines.length} lines (${format}) to clipboard`);
    } catch (e) {
      toast.show(`Export failed: ${e}`);
    } finally {
      exportInflight = false;
    }
  }

  function jsonlLine(r: Record<string, unknown>): string {
    return JSON.stringify(r) + '\n';
  }

  function plaintextLine(r: Record<string, unknown>, nickname: string | null): string {
    const ts = new Date(Number(r.ts_daemon_recv as bigint) * 1000)
      .toISOString().slice(0, 19).replace('T', ' ');
    const sender = (r.direction as string) === 'outgoing' ? 'Me' : (nickname ?? 'Peer');
    const kind = r.kind as Record<string, unknown> | null;
    const body = (kind && typeof kind === 'object' && 'body' in kind)
      ? String(kind.body)
      : `<${kind && 'kind' in kind ? String(kind.kind) : 'unknown'}>`;
    return `[${ts}] ${sender}: ${body}\n`;
  }

  async function runPrune() {
    if (!confirmPrune) return;
    const before = BigInt(Math.floor(Date.now() / 1000) - confirmPrune.older_than_days * 86400);
    confirmPrune = null;
    try {
      const resp = unwrapOk(await ipcClient.request({
        cmd: 'prune_history',
        contact: null,
        before_ts_recv: before,
        keep_last: null,
      }));
      if (resp.result !== 'pruned') {
        throw new Error(`unexpected reply: ${resp.result}`);
      }
      toast.show(`Pruned ${resp.data.rows_deleted} messages`);
    } catch (e) {
      toast.show(`Prune failed: ${e}`);
    }
  }
</script>

<h1>History</h1>

<section>
  <h2>Retention</h2>
  {#each PRESETS as p}
    <label class="preset">
      <input
        type="radio"
        name="retention"
        value={p.days}
        checked={snapshot?.history_retention_days === p.days}
        onchange={() => setRetention(p.days)}
      />
      {p.label}
    </label>
  {/each}
</section>

<section>
  <h2>Search</h2>
  <p class="muted">
    Cross-conversation search palette opens with <kbd>Cmd</kbd>/<kbd>Ctrl</kbd>+<kbd>K</kbd>
    from anywhere in the app. Inline search panel coming in a follow-up task.
  </p>
</section>

<section>
  <h2>Export</h2>
  <div class="export-controls">
    <label><input type="radio" name="format" bind:group={format} value="jsonl" /> JSONL</label>
    <label><input type="radio" name="format" bind:group={format} value="plaintext" /> Plaintext</label>
    <button type="button" onclick={downloadAll} disabled={exportInflight}>
      {exportInflight ? 'Exporting…' : 'Copy all conversations to clipboard'}
    </button>
  </div>
  <p class="muted">Exports all non-archived conversations. Result is copied to your clipboard.</p>
</section>

<section>
  <h2>Prune</h2>
  <div class="prune-controls">
    <label class="prune-row">
      Older than
      <input type="number" min="1" bind:value={pruneDays} />
      days
    </label>
    <button type="button" onclick={() => (confirmPrune = { older_than_days: pruneDays })}>
      Delete…
    </button>
  </div>
</section>

{#if confirmPrune}
  <ConfirmDialog
    title="Delete old messages?"
    body={`Permanently delete messages older than ${confirmPrune.older_than_days} days across all conversations. This cannot be undone.`}
    confirmLabel="Delete"
    danger={true}
    onConfirm={runPrune}
    onCancel={() => (confirmPrune = null)}
  />
{/if}

<style>
  section { margin-bottom: var(--s-3, 16px); }
  section h2 { font-size: 1.1em; margin: 0 0 var(--s-2, 12px) 0; }
  .preset { display: block; padding: var(--s-1, 6px) 0; cursor: pointer; }
  .muted { color: var(--text-muted, #888); font-size: 0.9em; margin-top: var(--s-1, 6px); }
  kbd {
    background: var(--bg-elevated, #1a1a1a);
    border: 1px solid var(--border, #2a2a2a);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: monospace;
    font-size: 0.85em;
  }
  .export-controls { display: flex; flex-wrap: wrap; gap: var(--s-2, 12px); align-items: center; }
  .prune-controls { display: flex; align-items: center; gap: var(--s-2, 12px); }
  .prune-row { display: inline-flex; align-items: center; gap: var(--s-1, 6px); }
  input[type="number"] { width: 5em; padding: 2px 4px; }
</style>
