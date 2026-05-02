// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Playwright e2e spec — first-run wizard happy path.
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// Flow: welcome → passphrase fill → confirm → reveal seed → type-back
//       → bootstrap → assert main shell renders.

import { test, expect } from "@playwright/test";

// The mock mnemonic (24 words) produced by tauri-mock.ts identity_init.
const MOCK_MNEMONIC =
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon " +
  "abandon abandon abandon abandon abandon art";

test.describe("first-run wizard happy path", () => {
  test.beforeEach(async ({ page }) => {
    // vault_exists returns false by default in the mock → wizard starts at welcome.
    await page.goto("/first-run");
  });

  test("welcome step renders and Continue advances to passphrase", async ({ page }) => {
    await expect(page.getByRole("heading", { name: "Welcome to Skattr" })).toBeVisible();
    await page.getByRole("button", { name: "Continue" }).click();
    await expect(page.getByRole("heading", { name: "Create a passphrase" })).toBeVisible();
  });

  test("full wizard: passphrase → seed type-back → bootstrap → main shell", async ({ page }) => {
    // ── Step 1: Welcome ──────────────────────────────────────────────────
    await expect(page.getByRole("heading", { name: "Welcome to Skattr" })).toBeVisible();
    await page.getByRole("button", { name: "Continue" }).click();

    // ── Step 2: Passphrase ───────────────────────────────────────────────
    await expect(page.getByRole("heading", { name: "Create a passphrase" })).toBeVisible();

    // Fill a strong passphrase (≥3/4 strength).  We bypass zxcvbn by
    // using a complex phrase that scores well; if the mock's strength
    // check is in the component, we fill both fields with a high-entropy
    // passphrase.  The Passphrase component checks pass === confirm and
    // strength >= 3 before calling identity_init.
    const strongPass = "Tr0ub4dor&3-TestOnlyPhrase!99";
    await page.getByPlaceholder("Passphrase").fill(strongPass);
    // Trigger oninput to evaluate strength.
    await page.getByPlaceholder("Passphrase").dispatchEvent("input");
    await page.getByPlaceholder("Confirm").fill(strongPass);
    await page.getByRole("button", { name: "Create identity" }).click();

    // ── Step 3: Seed phrase reveal ───────────────────────────────────────
    await expect(page.getByRole("heading", { name: "Save your seed phrase" })).toBeVisible();
    await page.getByRole("button", { name: "Reveal seed phrase" }).click();
    // The <pre> with the mnemonic should appear.
    const seedPre = page.locator("pre.seed");
    await expect(seedPre).toBeVisible({ timeout: 5_000 });

    // ── Step 4: Type-back confirmation ───────────────────────────────────
    await page.getByPlaceholder("word1 word2 word3 …").fill(MOCK_MNEMONIC);
    await page.getByRole("button", { name: "Confirm" }).click();

    // ── Step 5: Bootstrap / Tor connecting ───────────────────────────────
    // Bootstrap.svelte calls start_in_process_cmd then subscribes to
    // tor_status.  The mock fires Ready after 50 ms, which triggers
    // finishBootstrap() → ipc_request("daemon_info") → goto("/").
    await expect(page.getByRole("heading", { name: "Connecting to Tor" })).toBeVisible({ timeout: 5_000 });

    // ── Step 6: Main shell ───────────────────────────────────────────────
    // After Ready event + daemon_info, the router pushes "/".
    // The main shell has a .shell grid with a .rail aside.
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
  });
});
