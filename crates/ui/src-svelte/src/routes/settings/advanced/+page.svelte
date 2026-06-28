<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { config, fetchConfig, patchConfig, wipeAllData, exportBackup } from "$lib/stores/config";
  import { save, open } from "@tauri-apps/plugin-dialog";
  import { ipcClient } from "$lib/ipc/tauri";
  import { unwrapOk } from "$lib/ipc/client";
  import LogsViewer from "$lib/components/LogsViewer.svelte";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import { toast } from "$lib/stores/toast";

  let showLogs = $state(false);
  let confirmStage1 = $state(false);
  let confirmStage2 = $state(false);

  // daemon_info fields.
  let pubkey = $state<string | null>(null);
  let onion = $state<string | null>(null);
  let daemonVersion = $state<string | null>(null);
  let schemaVersion = $state<number | null>(null);

  onMount(async () => {
    await fetchConfig();
    try {
      const resp = await ipcClient.request({ cmd: "daemon_info" });
      const result = unwrapOk(resp);
      if (result.result !== "daemon_info") {
        throw new Error(`unexpected reply: ${result.result}`);
      }
      pubkey = result.data.local_pubkey;
      onion = result.data.current_onion;
      daemonVersion = result.data.daemon_version;
      schemaVersion = result.data.schema_version;
    } catch (e) {
      toast.show(`Failed to load daemon info: ${e}`);
    }
  });

  let snapshot = $derived($config.snapshot);

  /** Build a full ConfigPatch with only one field set; others null. */
  function singlePatch(field: string, value: unknown): Parameters<typeof patchConfig>[0] {
    return {
      history_retention_days: null,
      direct_timeout_secs: null,
      notification_mode: null,
      close_to_tray: null,
      start_minimised: null,
      persist_logs_to_disk: null,
      [field]: value,
    } as Parameters<typeof patchConfig>[0];
  }

  async function toggleCloseToTray(e: Event) {
    const v = (e.target as HTMLInputElement).checked;
    try {
      await patchConfig(singlePatch("close_to_tray", v));
      toast.show("Close-to-tray updated");
    } catch (err) {
      toast.show(`Failed: ${err}`);
    }
  }

  async function toggleStartMinimised(e: Event) {
    const v = (e.target as HTMLInputElement).checked;
    try {
      await patchConfig(singlePatch("start_minimised", v));
      toast.show("Start-minimised updated (effective on next launch)");
    } catch (err) {
      toast.show(`Failed: ${err}`);
    }
  }

  async function togglePersistLogs(e: Event) {
    const v = (e.target as HTMLInputElement).checked;
    try {
      await patchConfig(singlePatch("persist_logs_to_disk", v));
      toast.show(
        v
          ? "Logs will persist to disk on next daemon restart"
          : "Disk persistence off (existing files retained)",
      );
    } catch (err) {
      toast.show(`Failed: ${err}`);
    }
  }

  // Let the user choose where explicit Save… actions write their decrypted copy.
  // Received attachments stay encrypted at rest; plaintext is only produced
  // when the user clicks Open or Save…. Defaults to ~/Downloads when unset.
  async function chooseDownloadFolder() {
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: "Choose download folder",
      });
      if (typeof picked === "string" && picked.length > 0) {
        await patchConfig(singlePatch("download_dir", picked));
        toast.show(`Download folder set to ${picked}`);
      }
    } catch (e) {
      toast.show(`Couldn't set download folder: ${e}`);
    }
  }

  /**
   * Show the save-picker, run the export, and toast on success/failure.
   * Returns true only when an export SUCCEEDED; returns false on cancel or
   * failure. Callers must NOT advance to a destructive step unless true.
   */
  async function pickAndExport(): Promise<boolean> {
    try {
      const path = await save({
        defaultPath: "skattr-backup.age",
        filters: [{ name: "Skattr backup", extensions: ["age"] }],
      });
      if (!path) {
        // User cancelled the picker — do NOT advance.
        return false;
      }
      await exportBackup(path);
      toast.show("Backup saved.");
      return true;
    } catch (e) {
      // Export failed — show error, stay on current stage.
      toast.show(`Backup failed: ${e}`);
      return false;
    }
  }

  async function exportBackupAction() {
    await pickAndExport();
    // Return value intentionally ignored: the action button doesn't gate anything.
  }

  /**
   * Export backup then advance to stage 2 on success.
   * A cancelled file-picker (no path chosen) or a failed export stays on
   * stage 1 so the user can retry or choose another path.
   */
  async function exportBackupThenAdvance() {
    const exported = await pickAndExport();
    if (!exported) {
      // Cancelled or failed — do NOT advance.
      return;
    }
    // Only advance after a confirmed successful export.
    confirmStage1 = false;
    confirmStage2 = true;
  }

  let stage1Busy = $state(false);
  // Focus ref for the Cancel button in the stage-1 dialog (safe default focus
  // for a destructive modal — never focus the destructive action).
  let stage1CancelBtn = $state<HTMLButtonElement | null>(null);

  // Move focus to Cancel when the stage-1 dialog opens.
  $effect(() => {
    if (confirmStage1 && stage1CancelBtn) {
      stage1CancelBtn.focus();
    }
  });

  async function stage1ExportAndAdvance() {
    if (stage1Busy) return;
    stage1Busy = true;
    try {
      await exportBackupThenAdvance();
    } finally {
      stage1Busy = false;
    }
  }

  async function wipe() {
    confirmStage2 = false;
    try {
      await wipeAllData();
    } catch {
      // Connection close is expected after wipe; suppress.
    }
    toast.show("Skattr is wiping data and shutting down.");
  }

  async function copy(s: string) {
    try {
      await navigator.clipboard.writeText(s);
      toast.show("Copied");
    } catch (e) {
      toast.show(`Copy failed: ${e}`);
    }
  }
</script>

<h1>Advanced</h1>

<section>
  <h2>Behaviour</h2>
  <label class="toggle">
    <input
      type="checkbox"
      checked={snapshot?.close_to_tray ?? true}
      onchange={toggleCloseToTray}
    />
    Close button hides to tray
  </label>
  <label class="toggle">
    <input
      type="checkbox"
      checked={snapshot?.start_minimised ?? false}
      onchange={toggleStartMinimised}
    />
    Start minimised to tray (effective on next launch)
  </label>
</section>

<section>
  <h2>Files</h2>
  <p class="hint">
    Received attachments stay encrypted until you open or save them. When you
    click <strong>Save…</strong>, the decrypted copy goes to your
    <strong>Downloads</strong> folder by default — choose a different location
    here.
  </p>
  <button type="button" onclick={chooseDownloadFolder}>Choose save folder…</button>
</section>

<section>
  <h2>Logs</h2>
  <label class="toggle">
    <input
      type="checkbox"
      checked={snapshot?.persist_logs_to_disk ?? false}
      onchange={togglePersistLogs}
    />
    Persist logs to disk — rotated daily; effective on next daemon restart
  </label>
  <div class="logs-actions">
    <button type="button" onclick={() => (showLogs = !showLogs)}>
      {showLogs ? "Close" : "Open"} logs viewer
    </button>
  </div>
  {#if showLogs}
    <LogsViewer />
  {/if}
</section>

<section>
  <h2>Debug info</h2>
  <dl>
    <dt>Daemon version</dt>
    <dd>{daemonVersion ?? "(loading)"}</dd>
    <dt>Schema version</dt>
    <dd>{schemaVersion !== null ? String(schemaVersion) : "(loading)"}</dd>
    <dt>Public key</dt>
    <dd>
      {#if pubkey}
        <code>{pubkey.slice(0, 16)}…</code>
        <button type="button" onclick={() => copy(pubkey!)}>Copy</button>
      {:else}
        <span class="muted">(loading)</span>
      {/if}
    </dd>
    <dt>Onion</dt>
    <dd>
      {#if onion}
        <code>{onion.slice(0, 16)}…</code>
        <button type="button" onclick={() => copy(onion!)}>Copy</button>
      {:else}
        <span class="muted">(loading)</span>
      {/if}
    </dd>
  </dl>
</section>

<section>
  <h2>Backup</h2>
  <p class="backup-desc">Export an encrypted backup of your database (.age file). Restore by replacing your data directory with the decrypted contents.</p>
  <button type="button" onclick={exportBackupAction}>Export backup…</button>
</section>

<section class="danger-zone">
  <h2>Danger zone</h2>
  <p>Permanently removes all contacts, messages, mailboxes, identity, and the database.</p>
  <button type="button" class="danger-btn" onclick={() => (confirmStage1 = true)}>
    Delete all data and quit
  </button>
</section>

{#if confirmStage1}
  <!-- Bespoke three-button stage-1 dialog (ConfirmDialog supports only two buttons). -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="overlay"
    role="dialog"
    aria-modal="true"
    aria-labelledby="wipe-stage1-title"
    onkeydown={(e) => { if (e.key === "Escape" && !stage1Busy) confirmStage1 = false; }}
  >
    <div class="dialog">
      <h2 id="wipe-stage1-title">Delete all Skattr data?</h2>
      <p>This permanently removes contacts, messages, mailboxes, identity, and the database. This cannot be undone. Export a backup first if you want to keep a copy.</p>
      <div class="actions">
        <button type="button" bind:this={stage1CancelBtn} onclick={() => (confirmStage1 = false)} disabled={stage1Busy}>
          Cancel
        </button>
        <button
          type="button"
          class="danger-btn"
          onclick={() => {
            confirmStage1 = false;
            confirmStage2 = true;
          }}
          disabled={stage1Busy}
        >
          Continue without backup
        </button>
        <button
          type="button"
          onclick={stage1ExportAndAdvance}
          disabled={stage1Busy}
        >
          {stage1Busy ? "Saving…" : "Export backup first"}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if confirmStage2}
  <ConfirmDialog
    title="Are you absolutely sure?"
    body="This is final. Skattr will wipe its data directory and exit immediately. All messages and identity will be gone forever."
    confirmLabel="Wipe everything"
    danger
    onConfirm={wipe}
    onCancel={() => (confirmStage2 = false)}
  />
{/if}

<style>
  h1 {
    font: var(--t-display);
    margin: 0 0 var(--s-3);
  }
  section {
    margin-bottom: var(--s-3);
  }
  section h2 {
    font-size: 1.1em;
    margin: 0 0 var(--s-2);
    color: var(--text);
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-1) 0;
    cursor: pointer;
    font: var(--t-body);
  }
  .logs-actions {
    margin-top: var(--s-2);
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--s-1) var(--s-3);
    margin: 0;
  }
  dt {
    color: var(--text-muted);
    font: var(--t-ui);
    align-self: center;
  }
  dd {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  code {
    font-family: monospace;
    background: var(--bg);
    padding: 2px 6px;
    border-radius: 3px;
  }
  .muted {
    color: var(--text-muted);
  }
  .backup-desc {
    font: var(--t-body);
    color: var(--text-muted);
    margin: 0 0 var(--s-2);
  }
  .danger-zone {
    border: 1px solid var(--danger);
    border-radius: 6px;
    padding: var(--s-2) var(--s-3);
  }
  .danger-zone h2 {
    color: var(--danger);
  }
  .danger-zone p {
    font: var(--t-body);
    color: var(--text-muted);
    margin: 0 0 var(--s-2);
  }
  .danger-btn {
    background: var(--danger);
    color: white;
    border: none;
    padding: 8px 16px;
    border-radius: 4px;
    cursor: pointer;
  }
  .danger-btn:hover {
    filter: brightness(1.15);
  }
  button {
    padding: 6px 12px;
    cursor: pointer;
  }
  /* Three-button wipe stage-1 dialog */
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
  .dialog h2 {
    font: var(--t-display);
    margin: 0 0 var(--s-2);
  }
  .dialog p {
    font: var(--t-body);
    margin: 0 0 var(--s-3);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--s-2);
    flex-wrap: wrap;
  }
</style>
