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
  /* #197: this icon is only ever rendered inside an accent-filled outgoing
     bubble, so page-background tokens do not apply — --text-muted was 1.05:1
     there and --accent (delivered) was 1.00:1, i.e. invisible. Inherit the
     bubble's own foreground instead and let the icon SHAPE carry the state,
     which also keeps the distinction off colour alone. */
  .pending  :global(svg) { color: currentColor; opacity: 0.8; }
  .sent     :global(svg) { color: currentColor; opacity: 0.8; }
  .delivered :global(svg) { color: currentColor; }
  .failed   :global(svg) { color: var(--danger-on-accent); }
</style>
