<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz B.V. -->
<script lang="ts">
  let { onNext }: { onNext: () => void } = $props();
</script>

<section class="step">
  <h1>Welcome to Skattr</h1>
  <p>
    Skattr is a metadata-resistant peer-to-peer messenger. All traffic is
    routed over Tor v3 onion services. Messages are encrypted with MLS.
  </p>
  <h2>What this protects</h2>
  <ul>
    <li>Message contents (end-to-end encrypted, MLS).</li>
    <li>Network metadata (Tor onion routing).</li>
    <li>Server-side records (no central server).</li>
  </ul>
  <h2>What this does not protect</h2>
  <ul>
    <li>A compromised endpoint device.</li>
    <li>Side-channel attacks on your installation.</li>
    <li>Loss of your seed phrase or passphrase.</li>
  </ul>
  <button onclick={onNext}>Continue</button>
</section>

<style>
  .step { max-width: 60ch; margin: var(--s-4) auto; padding: var(--s-3); }
  button {
    background: var(--accent);
    color: var(--bg);
    padding: var(--s-2) var(--s-3);
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font: var(--t-body);
  }
</style>
