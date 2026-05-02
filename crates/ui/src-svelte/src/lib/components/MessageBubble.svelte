<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import type { MessageRecord } from "$lib/ipc/types";

  let { record }: { record: MessageRecord } = $props();

  // Kind shape: { kind: "text", body: string } | { kind: "file", ... } | ...
  let body = $derived(
    record.kind && record.kind.kind === "text" ? record.kind.body : "",
  );
  let isOutgoing = $derived(record.direction === "outgoing");
  // ts_daemon_recv is bigint in the wire type; coerce to number for Date.
  let tsMs = $derived(Number(record.ts_daemon_recv) * 1000);
</script>

<div class="bubble" class:outgoing={isOutgoing}>
  <p class="body">{body}</p>
  <time class="ts">{new Date(tsMs).toLocaleTimeString()}</time>
</div>

<style>
  .bubble {
    background: var(--bg-elevated);
    color: var(--text);
    padding: var(--s-2) var(--s-3);
    border-radius: 12px;
    margin: var(--s-1) 0;
    max-width: 60ch;
  }
  .bubble.outgoing { background: var(--accent); color: var(--bg); margin-left: auto; }
  .body { margin: 0; white-space: pre-wrap; word-break: break-word; }
  .ts { color: var(--text-muted); font: var(--t-ui); display: block; margin-top: var(--s-1); }
</style>
