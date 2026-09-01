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
 *  Deliberately does NOT wait: the caller waits for the CONDITION it expects
 *  (#224). A fixed budget here has to cover IntersectionObserver + loadOlder +
 *  the mock'"'"'s 80 ms delay, and under concurrent workers 300 ms did not, which
 *  failed this spec about one full run in two. */
async function wheelToTop(page: import("@playwright/test").Page): Promise<void> {
  await page.locator(".list").hover();
  await page.mouse.wheel(0, -999_999);
}

/** Settle time allowed for an UNEXPECTED extra page to show up. */
const OVERSHOOT_WINDOW_MS = 400;

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
    // Wait until loading stabilises before scrolling. This one is a genuine
    // "nothing more should happen" wait, so it stays a fixed budget — but a
    // generous one, since it gates every assertion after it.
    await page.waitForTimeout(1_000);
    const afterCascade = await messageCount(page);
    // At most 2 pages can auto-cascade (beyond that content fills viewport).
    expect(afterCascade).toBeGreaterThanOrEqual(50);
    expect(afterCascade).toBeLessThanOrEqual(100);

    // Scroll to bottom so the top sentinel leaves the viewport.
    await wheelToBottom(page);

    // Load remaining pages one at a time by scrolling to the top.
    let current = await messageCount(page);
    while (current < 200) {
      const expected = Math.min(current + 50, 200);
      await wheelToTop(page);
      // Wait for the page to actually arrive rather than for a wall clock —
      // this fails loudly (and legibly) if it never does.
      await waitForCount(page, expected, 5_000);
      // Then confirm exactly ONE page arrived. Without this, a scroll that
      // cascaded two pages would slip through on the transient match — which
      // is the #222 regression this loop exists to catch.
      await page.waitForTimeout(OVERSHOOT_WINDOW_MS);
      const next = await messageCount(page);
      expect(next).toBe(expected);
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
    await page.waitForTimeout(OVERSHOOT_WINDOW_MS);
    expect(await messageCount(page)).toBe(200);

    // Virtualizer should render only the visible subset (not 200 DOM nodes).
    const domBubbles = await page.locator(".bubble").count();
    expect(domBubbles).toBeLessThan(200);
    expect(domBubbles).toBeGreaterThan(0);
  });
});
