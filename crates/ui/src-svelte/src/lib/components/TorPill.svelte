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

  type Phase = "idle" | "connecting" | "ready" | "failed";

  let phase = $derived.by<Phase>(() => {
    const s = $torStatus;
    if (s === "Ready") return "ready";
    if (s !== null && typeof s === "object") {
      return "Bootstrapping" in s ? "connecting" : "failed";
    }
    return "idle";
  });

  let label = $derived.by(() => {
    switch (phase) {
      case "ready":      return "Tor connected";
      case "connecting": return `Connecting ${bootstrapPct($torStatus)}%`;
      case "failed":     return "Tor failed";
      default:           return "Disconnected";
    }
  });

  // How many of the three hops are drawn as established. Bootstrap percentage
  // is not a hop count — it is the only progress the daemon reports, so it is
  // shown as one, coarsely, alongside the exact number in the label.
  let hops = $derived.by(() => {
    if (phase === "ready") return 3;
    if (phase !== "connecting") return 0;
    const pct = bootstrapPct($torStatus);
    return pct >= 66 ? 2 : pct >= 33 ? 1 : 0;
  });
</script>

<!-- The circuit is the one ornament in the shell: in a messenger whose premise
     is a hostile network, its health is the most characteristic thing on
     screen. State is carried by the label as well as the colour. -->
<div class="circuit {phase}" title={phase === "failed" ? failMsg($torStatus) : undefined}>
  <span class="node" class:on={hops >= 1}></span>
  <span class="wire" class:on={hops >= 2}><i></i></span>
  <span class="node" class:on={hops >= 2}></span>
  <span class="wire" class:on={hops >= 3}><i></i></span>
  <span class="node" class:on={hops >= 3}></span>
  <span class="label">{label}</span>
</div>

<style>
  .circuit {
    display: inline-flex;
    align-items: center;
    /* Nodes and wires meet edge to edge — the trace has to read as one line. */
    gap: 0;
    color: var(--text-muted);
  }
  .circuit.ready { color: var(--live); }
  .circuit.connecting { color: var(--accent); }
  .circuit.failed { color: var(--danger); }

  .node {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: 1px solid currentColor;
    flex: none;
  }
  .node.on { background: currentColor; }
  /* The only glow in the app, on the only thing that is genuinely live. */
  .circuit.ready .node.on { box-shadow: 0 0 0 3px var(--live-glow); }

  .wire {
    position: relative;
    width: 26px;
    height: 1px;
    background: var(--hairline);
    overflow: hidden;
  }
  .wire.on { background: currentColor; }

  /* The travelling pulse only exists on a live circuit — motion here means
     traffic, so it must not run while nothing is connected. */
  .wire i { display: none; }
  .circuit.ready .wire i {
    display: block;
    position: absolute;
    top: -1px;
    left: -12px;
    width: 12px;
    height: 3px;
    border-radius: 2px;
    background: linear-gradient(90deg, transparent, currentColor);
    animation: travel 2.6s linear infinite;
  }
  /* Second wire only: nodes and wires are both spans, so nth-of-type would
     not tell them apart. */
  .circuit.ready > span:nth-child(4) i { animation-delay: 0.32s; }

  @keyframes travel {
    0%   { transform: translateX(0); opacity: 0; }
    12%  { opacity: 1; }
    70%  { opacity: 1; }
    100% { transform: translateX(38px); opacity: 0; }
  }
  @media (prefers-reduced-motion: reduce) {
    .circuit.ready .wire i { animation: none; opacity: 0; }
  }

  .label {
    font: var(--t-label);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-left: var(--s-2);
    white-space: nowrap;
  }
</style>
