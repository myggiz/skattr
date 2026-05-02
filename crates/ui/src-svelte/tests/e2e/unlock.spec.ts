// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Playwright e2e spec — existing-vault unlock path.
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// The mock pre-flags _vault=true when ?vault=yes is in the URL.
// Flow: /first-run?vault=yes → onMount detects vault_exists=true → unlock step
//       → enter passphrase → bootstrap → main shell.

import { test, expect } from "@playwright/test";

test.describe("existing-vault unlock path", () => {
  test.beforeEach(async ({ page }) => {
    // ?vault=yes triggers _vault=true in tauri-mock so vault_exists returns true.
    await page.goto("/first-run?vault=yes");
  });

  test("wizard enters unlock mode when vault exists", async ({ page }) => {
    // The +page.svelte onMount calls vault_exists → true → step = "unlock".
    // Unlock mode renders Passphrase with mode="unlock" → heading "Unlock".
    await expect(page.getByRole("heading", { name: "Unlock" })).toBeVisible({ timeout: 5_000 });
    // Create identity button must NOT be visible (this is unlock mode).
    await expect(page.getByRole("button", { name: "Create identity" })).not.toBeVisible();
    // The single passphrase field should be present (no confirm in unlock mode).
    await expect(page.getByPlaceholder("Passphrase")).toBeVisible();
  });

  test("unlock: passphrase → bootstrap → main shell", async ({ page }) => {
    // ── Step 1: Unlock ────────────────────────────────────────────────────
    await expect(page.getByRole("heading", { name: "Unlock" })).toBeVisible({ timeout: 5_000 });

    // vault_unlock in the mock succeeds for any passphrase when _vault=true.
    await page.getByPlaceholder("Passphrase").fill("any-passphrase");
    await page.getByRole("button", { name: "Unlock" }).click();

    // ── Step 2: Bootstrap / Tor connecting ──────────────────────────────
    await expect(page.getByRole("heading", { name: "Connecting to Tor" })).toBeVisible({ timeout: 5_000 });

    // ── Step 3: Main shell ───────────────────────────────────────────────
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
  });
});
