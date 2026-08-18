// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Playwright e2e spec — #178: the contact row is a toggle.
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// Uses ?fixture=seeded-contact (same as composer.spec.ts): vault_exists is
// true and list_contacts returns a single "test peer".

import { test, expect } from "@playwright/test";

test.describe("conversation toggle (#178)", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?fixture=seeded-contact");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
  });

  test("clicking the active contact closes the conversation, clicking again reopens it", async ({
    page,
  }) => {
    const row = page.locator(".rail .row").first();
    const empty = page.locator("p.empty");

    // Nothing open yet: the neutral state is showing.
    await expect(empty).toBeVisible();

    // Open it — the composer is the marker that a conversation is loaded.
    await row.click();
    await expect(page.getByLabel("Message input")).toBeVisible();
    await expect(empty).toHaveCount(0);
    await expect(row).toHaveClass(/active/);

    // Click the SAME contact: it closes and returns to the neutral state.
    // This is the behaviour #178 asks for — a user can leave the app open
    // with no conversation content on screen.
    await row.click();
    await expect(empty).toBeVisible();
    await expect(page.getByLabel("Message input")).toHaveCount(0);
    await expect(row).not.toHaveClass(/active/);

    // And it reopens, so the toggle is not one-way.
    await row.click();
    await expect(page.getByLabel("Message input")).toBeVisible();
  });
});
