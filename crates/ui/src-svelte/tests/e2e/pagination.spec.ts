// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Playwright e2e spec — Scroll-back pagination.
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// Setup: navigating to /?fixture=seeded-200-msgs makes tauri-mock return
// a single "pagination peer" contact with 200 pre-seeded messages.
// The mock serves them in pages of 50 (DESC by row_id) with an 80 ms
// artificial delay per page.
//
// The VirtualMessageList exposes a data-message-count attribute on .list
// that reflects the number of messages held in the store (not DOM rows,
// since the virtualizer renders only visible items).  Tests assert this
// attribute to track pagination progress independently of DOM row count.

import { test, expect } from "@playwright/test";

/** Return the current data-message-count from .list. */
async function messageCount(page: import("@playwright/test").Page): Promise<number> {
  return page.evaluate(() => {
    const el = document.querySelector<HTMLElement>(".list");
    return parseInt(el?.dataset.messageCount ?? "0", 10);
  });
}

/** Wait until data-message-count reaches `target` (up to `timeout` ms). */
async function waitForCount(
  page: import("@playwright/test").Page,
  target: number,
  timeout = 3_000,
): Promise<void> {
  await expect
    .poll(async () => messageCount(page), { timeout })
    .toBe(target);
}

/** Scroll the list to its bottom using mouse wheel and wait for the
 *  IntersectionObserver to process the new position. */
async function wheelToBottom(page: import("@playwright/test").Page): Promise<void> {
  await page.locator(".list").hover();
  await page.mouse.wheel(0, 999_999);
  await page.waitForTimeout(150);
}

/** Scroll the list to the very top using mouse wheel.
 *  The mock resolves with 80 ms delay so we give IO time to fire. */
async function wheelToTop(page: import("@playwright/test").Page): Promise<void> {
  await page.locator(".list").hover();
  await page.mouse.wheel(0, -999_999);
  // Allow IntersectionObserver + loadOlder + 80 ms mock delay to complete.
  await page.waitForTimeout(300);
}

test.describe("conversation pagination", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?fixture=seeded-200-msgs");
    // Wait for the main shell to render before interacting.
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    // Open the seeded conversation (ContactRow renders as .rail .row).
    await page.locator(".rail .row").first().click();
    await expect(page.locator(".list")).toBeVisible();
  });

  test("scroll-to-top loads older pages until cursor exhausts", async ({ page }) => {
    // Wait for first page (50 messages) to load.
    await waitForCount(page, 50);

    // After the first page the list may be shorter than the viewport, causing
    // the top-sentinel IO to fire immediately and cascade-load page 2.
    // Wait until loading stabilises before scrolling.
    await page.waitForTimeout(300);
    const afterCascade = await messageCount(page);
    // At most 2 pages can auto-cascade (beyond that content fills viewport).
    expect(afterCascade).toBeGreaterThanOrEqual(50);
    expect(afterCascade).toBeLessThanOrEqual(100);

    // Scroll to bottom so the top sentinel leaves the viewport.
    await wheelToBottom(page);

    // Load remaining pages one at a time by scrolling to the top.
    let current = await messageCount(page);
    while (current < 200) {
      await wheelToTop(page);
      const next = await messageCount(page);
      // Each scroll-to-top must advance by exactly one page (50 rows).
      expect(next).toBe(Math.min(current + 50, 200));
      current = next;
      if (current < 200) {
        await wheelToBottom(page);
      }
    }

    // All 200 messages are now loaded.
    expect(await messageCount(page)).toBe(200);

    // Cursor is exhausted (next_before_id = null).
    // A final scroll-to-top must not trigger any further fetches.
    await wheelToBottom(page);
    await wheelToTop(page);
    await page.waitForTimeout(400);
    expect(await messageCount(page)).toBe(200);

    // Virtualizer should render only the visible subset (not 200 DOM nodes).
    const domBubbles = await page.locator(".bubble").count();
    expect(domBubbles).toBeLessThan(200);
    expect(domBubbles).toBeGreaterThan(0);
  });
});
