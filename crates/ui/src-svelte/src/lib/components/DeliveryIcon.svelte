<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { icons } from "$lib/icons";

  type Status = "pending" | "sent" | "delivered" | "failed";

  let { status, title }: { status: Status; title?: string } = $props();

  const glyph = $derived(
    status === "pending"   ? icons["clock"]
  : status === "sent"      ? icons["check"]
  : status === "delivered" ? icons["check-check"]
                           : icons["alert-triangle"],
  );
</script>

<span
  class="icon"
  class:pending={status === "pending"}
  class:sent={status === "sent"}
  class:delivered={status === "delivered"}
  class:failed={status === "failed"}
  title={title ?? undefined}
>
  {@html glyph}
</span>

<style>
  .icon {
    display: inline-flex;
    align-items: center;
    width: 14px;
    height: 14px;
    margin-left: var(--s-1);
    vertical-align: middle;
  }
  .icon :global(svg) {
    width: 14px;
    height: 14px;
  }
  .pending  :global(svg) { color: var(--text-muted); }
  .sent     :global(svg) { color: var(--text-muted); }
  .delivered :global(svg) { color: var(--accent); }
  .failed   :global(svg) { color: var(--danger); }
</style>
