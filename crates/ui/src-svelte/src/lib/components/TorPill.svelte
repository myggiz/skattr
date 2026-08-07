<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  import { torStatus } from "$lib/stores/tor_status";
  import type { TorStatus } from "$lib/ipc/types";

  // TorStatus shape: "Idle" | { Bootstrapping: number } | "Ready" | { Failed: string }
  function bootstrapPct(s: TorStatus | null): number {
    if (s !== null && typeof s === "object" && "Bootstrapping" in s) {
      return s.Bootstrapping;
    }
    return 0;
  }

  function failMsg(s: TorStatus | null): string {
    if (s !== null && typeof s === "object" && "Failed" in s) {
      return s.Failed;
    }
    return "";
  }
</script>

<div class="pill">
  {#if $torStatus === null || $torStatus === "Idle"}
    <span class="dot grey"></span> Disconnected
  {:else if typeof $torStatus === "object" && "Bootstrapping" in $torStatus}
    <span class="dot grey"></span> Connecting ({bootstrapPct($torStatus)}%)
  {:else if $torStatus === "Ready"}
    <span class="dot accent"></span> Tor connected
  {:else if typeof $torStatus === "object" && "Failed" in $torStatus}
    <span class="dot danger" title={failMsg($torStatus)}></span> Failed
  {/if}
</div>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: var(--s-1);
    padding: var(--s-1) var(--s-2);
    background: var(--bg-elevated);
    border-radius: 999px;
    font: var(--t-ui);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    display: inline-block;
  }
  .grey { background: var(--text-muted); }
  .accent { background: var(--accent); }
  .danger { background: var(--danger); }
</style>
