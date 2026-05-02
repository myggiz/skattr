<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";

  import { ipcClient } from "$lib/ipc/tauri";
  import { torStatus } from "$lib/stores/tor_status";
  import { refreshContacts } from "$lib/stores/contacts";
  import { daemonInfo } from "$lib/stores/daemon_info";
  import { unwrapOk } from "$lib/ipc/client";
  import type { TorStatus } from "$lib/ipc/types";

  let error = $state<string | null>(null);
  // Track whether we've already initiated the finish sequence to avoid
  // duplicate calls if the event fires more than once.
  let finishing = false;

  function isReady(ts: TorStatus | null): boolean {
    return ts === "Ready";
  }

  function bootstrapPct(ts: TorStatus | null): number {
    if (ts !== null && typeof ts === "object" && "Bootstrapping" in ts) {
      return ts.Bootstrapping;
    }
    return 0;
  }

  async function finishBootstrap() {
    if (finishing) return;
    finishing = true;
    try {
      const resp = await ipcClient.request({ cmd: "daemon_info" });
      const r = unwrapOk(resp);
      // CommandResult is adjacent-tagged: { result: "daemon_info", data: { ... } }
      if (r.result === "daemon_info") {
        daemonInfo.set(r);
      }
      await refreshContacts();
      goto("/");
    } catch (e) {
      error = String(e);
      finishing = false;
    }
  }

  onMount(async () => {
    try {
      await invoke("start_in_process_cmd");
      // Subscribe TorStatus so cached replay paints the progress bar.
      await ipcClient.subscribe({ filter: "tor_status" }, (e) => {
        // Event is adjacent-tagged: { event: "tor_status_changed", data: TorStatus }
        if (e.event === "tor_status_changed") {
          torStatus.set(e.data);
          if (e.data === "Ready") {
            finishBootstrap();
          }
        }
      });
    } catch (e) {
      error = String(e);
    }
  });
</script>

<section class="step">
  <h1>Connecting to Tor</h1>
  {#if error}
    <p class="error">{error}</p>
  {:else if $torStatus !== null && typeof $torStatus === "object" && "Bootstrapping" in $torStatus}
    <progress max="100" value={bootstrapPct($torStatus)}></progress>
    <p>{bootstrapPct($torStatus)}%</p>
  {:else if isReady($torStatus)}
    <p>Connected. Loading…</p>
  {:else}
    <p>Starting…</p>
  {/if}
</section>

<style>
  .step {
    max-width: 40ch;
    margin: var(--s-4) auto;
    padding: var(--s-3);
    text-align: center;
  }
  progress { width: 100%; }
  .error { color: var(--danger); }
</style>
