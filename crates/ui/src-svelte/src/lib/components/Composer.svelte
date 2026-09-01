<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { send, sendFile } from "$lib/stores/conversation";
  import { open as pickFile } from "@tauri-apps/plugin-dialog";
  import { invoke } from "@tauri-apps/api/core";
  import { toast } from "$lib/stores/toast";
  import { MANIFEST_SIZE_HARD, MANIFEST_SIZE_SOFT, formatBytes } from "$lib/attachments";
  import { icons } from "$lib/icons";
  import type { PublicKey } from "$lib/ipc/types";

  let {
    contact,
    disabled,
    disabledReason,
  }: {
    contact: PublicKey;
    disabled: boolean;
    disabledReason?: string;
  } = $props();

  let text = $state("");
  let composing = $state(false);
  let textarea = $state<HTMLTextAreaElement | undefined>(undefined);

  async function trySend(): Promise<void> {
    const trimmed = text.trim();
    if (!trimmed || disabled) return;
    text = "";
    await send(contact, trimmed);
  }

  function onKeyDown(e: KeyboardEvent): void {
    if (e.key !== "Enter") return;
    if (e.shiftKey) return;
    if (e.isComposing || composing) return;
    e.preventDefault();
    void trySend();
  }

  async function tryAttach(): Promise<void> {
    if (disabled) return;
    let selected: string | string[] | null;
    try {
      selected = await pickFile({ multiple: false, directory: false });
    } catch (e) {
      toast.show("Could not open file picker");
      return;
    }
    if (selected === null || Array.isArray(selected)) return; // cancelled
    const path = selected;
    let size: number;
    try {
      size = await invoke<number>("file_size", { path });
    } catch {
      toast.show("File is unavailable");
      return;
    }
    if (size > MANIFEST_SIZE_HARD) {
      toast.show(`File too large (max ${formatBytes(MANIFEST_SIZE_HARD)})`);
      return;
    }
    if (size > MANIFEST_SIZE_SOFT) {
      const ok = window.confirm(
        `This file is ${formatBytes(size)}. It will only be delivered while your contact is online. Send anyway?`,
      );
      if (!ok) return;
    }
    const filename = path.split(/[/\\]/).pop() ?? "attachment";
    await sendFile(contact, path, filename, size);
  }

  function onPaste(e: ClipboardEvent): void {
    if (!e.clipboardData) return;
    e.preventDefault();
    const plain = e.clipboardData.getData("text/plain");
    if (!plain) return;
    const start = textarea?.selectionStart ?? text.length;
    const end = textarea?.selectionEnd ?? text.length;
    text = text.slice(0, start) + plain + text.slice(end);
    queueMicrotask(() => {
      if (textarea) {
        textarea.selectionStart = start + plain.length;
        textarea.selectionEnd = start + plain.length;
      }
    });
  }
</script>

<form class="composer" onsubmit={(e) => { e.preventDefault(); void trySend(); }}>
  <textarea
    bind:this={textarea}
    bind:value={text}
    {disabled}
    placeholder={disabled ? (disabledReason ?? "Disabled") : "Type a message"}
    rows={1}
    onkeydown={onKeyDown}
    onpaste={onPaste}
    oncompositionstart={() => (composing = true)}
    oncompositionend={() => (composing = false)}
    aria-label="Message input"
  ></textarea>
  <button
    type="button"
    class="attach"
    {disabled}
    onclick={() => void tryAttach()}
    aria-label="Attach file"
    title="Attach file"
  >{@html icons["paperclip"]}</button>
  <button type="submit" {disabled} aria-label="Send">Send</button>
</form>

<style>
  .composer {
    display: flex;
    align-items: flex-end;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border-top: 1px solid var(--hairline);
  }
  textarea {
    flex: 1;
    resize: none;
    min-height: 2.5rem;
    max-height: 8rem;
    padding: var(--s-2);
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    font: inherit;
  }
  textarea:focus { outline: none; border-color: var(--accent); }
  textarea:disabled { opacity: 0.5; cursor: not-allowed; }
  button {
    padding: var(--s-2) var(--s-3);
    background: var(--accent);
    color: var(--bg);
    border: 1px solid var(--accent);
    border-radius: 4px;
    font: var(--t-ui);
    cursor: pointer;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .attach {
    display: inline-flex;
    align-items: center;
    padding: var(--s-2);
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--hairline);
    border-radius: 4px;
    cursor: pointer;
  }
  .attach:hover:not(:disabled) { color: var(--text); border-color: var(--accent); }
  .attach :global(svg) { width: 18px; height: 18px; }
  .attach:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
