<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Myggiz AB -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  import Welcome from "./Welcome.svelte";
  import Passphrase from "./Passphrase.svelte";
  import SeedPhrase from "./SeedPhrase.svelte";
  import Bootstrap from "./Bootstrap.svelte";

  type WizardStep = "welcome" | "passphrase" | "seed" | "bootstrap" | "unlock";

  let step = $state<WizardStep>("welcome");
  let mnemonic = $state<string | null>(null);

  onMount(async () => {
    const exists = await invoke<boolean>("vault_exists");
    if (exists) step = "unlock";
  });

  function next(payload?: { mnemonic?: string }) {
    if (step === "welcome") {
      step = "passphrase";
    } else if (step === "passphrase") {
      mnemonic = payload?.mnemonic ?? null;
      step = "seed";
    } else if (step === "seed") {
      step = "bootstrap";
    }
  }
</script>

{#if step === "welcome"}
  <Welcome onNext={() => next()} />
{:else if step === "passphrase"}
  <Passphrase onNext={(m) => next({ mnemonic: m })} />
{:else if step === "seed"}
  <SeedPhrase {mnemonic} onNext={() => next()} />
{:else if step === "bootstrap"}
  <Bootstrap />
{:else if step === "unlock"}
  <Passphrase mode="unlock" onNext={() => (step = "bootstrap")} />
{/if}
