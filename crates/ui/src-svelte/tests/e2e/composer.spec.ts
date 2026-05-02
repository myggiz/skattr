// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
// Playwright e2e spec — Composer happy path.
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// Setup: navigating to /?fixture=seeded-contact makes tauri-mock return
// a pre-seeded contact from list_contacts and handle send_message with a
// deferred delivery_status_changed event (Queued → Delivered after 200 ms).

import { test, expect } from "@playwright/test";

test.describe("composer happy path", () => {
  test.beforeEach(async ({ page }) => {
    // ?fixture=seeded-contact activates the fixture branch in tauri-mock:
    // - vault_exists returns true (skips first-run redirect)
    // - list_contacts returns a single "test peer" contact
    // - send_message succeeds and fires delivery_status_changed after 200 ms
    await page.goto("/?fixture=seeded-contact");
    // Wait for the main shell to render before interacting.
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
  });

  test("Enter sends message; optimistic bubble shows pending icon, then promotes to delivered", async ({ page }) => {
    // Click the seeded contact in the rail to open the conversation.
    // ContactRow renders as <button class="row"> inside <aside class="rail">.
    await page.locator(".rail .row").first().click();

    const composer = page.getByLabel("Message input");
    await expect(composer).toBeVisible();
    await composer.fill("hello");
    await composer.press("Enter");

    // The optimistic bubble should appear immediately with the message body.
    const bubble = page.locator(".bubble.outgoing").last();
    await expect(bubble).toContainText("hello");

    // Pending state: DeliveryIcon renders with class "pending" (clock icon).
    await expect(bubble.locator(".icon.pending")).toBeVisible({ timeout: 1_000 });

    // After ~200 ms the mock emits delivery_status_changed with status "Delivered".
    // +page.svelte now forwards that event to recordDeliveryStatus, which
    // causes DeliveryIcon to re-render with class "delivered" (check-check icon).
    await expect(bubble.locator(".icon.delivered")).toBeVisible({ timeout: 2_000 });
  });

  test("Shift+Enter inserts newline without sending", async ({ page }) => {
    await page.locator(".rail .row").first().click();

    const composer = page.getByLabel("Message input");
    await expect(composer).toBeVisible();
    await composer.fill("hi");
    await composer.press("Shift+Enter");
    await composer.type("world");

    // Textarea value should contain a newline.
    expect(await composer.inputValue()).toBe("hi\nworld");
    // No outgoing bubble should have been added.
    await expect(page.locator(".bubble.outgoing")).toHaveCount(0);
  });
});
