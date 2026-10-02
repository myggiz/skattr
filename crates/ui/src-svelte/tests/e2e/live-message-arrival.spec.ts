// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Playwright e2e spec — a message arriving on the event stream must become
// VISIBLE in the already-open conversation (#230, relates to #214/#220/#222).
// Requires: TAURI_MOCK=1 pnpm test:e2e
//
// Why this lives in e2e and not vitest: the failure mode is POSITIONING. The
// virtualizer places rows by absolute offset computed from measured heights,
// and vitest runs in jsdom, which performs no layout — every element measures
// 0x0. `VirtualMessageList.test.ts` can prove the appended row exists in the
// DOM and that the virtualizer instance survived; it structurally cannot prove
// the row landed anywhere a human can see. That blind spot is why a fix for
// this shipped green twice and still failed in the field.
//
// Hence `toBeInViewport`, not `toBeVisible`: a row positioned outside the
// scroll viewport IS "visible" by CSS, and is exactly the field symptom
// ("the message never arrived", until the conversation is reopened).

import { test, expect, type Page } from "@playwright/test";

/** The pagination fixture's peer — 200 seeded incoming messages. */
const PEER_PUBKEY = "ef".repeat(32);

/** Row id for the injected arrival: one past the 200 seeded rows. */
const ARRIVAL_ROW_ID = 201;
const ARRIVAL_BODY = "how are you";

declare global {
  interface Window {
    __skattrEmit?: (msg: unknown) => void;
  }
}

/** Current data-message-count from .list (store count, not DOM rows). */
async function messageCount(page: Page): Promise<number> {
  return page.evaluate(() => {
    const el = document.querySelector<HTMLElement>(".list");
    return parseInt(el?.dataset.messageCount ?? "0", 10);
  });
}

/** Scroll the list to the bottom — the reading position the field report had. */
async function wheelToBottom(page: Page): Promise<void> {
  await page.locator(".list").hover();
  await page.mouse.wheel(0, 999_999);
  await page.waitForTimeout(150);
}

/**
 * Push a `message_received` through the REAL subscribe path.
 *
 * Only the IPC transport is mocked; the client, the `+page.svelte` dispatcher,
 * `appendMessage`, and the whole component render still run — so this exercises
 * every layer the defect could be in, not the seam under test.
 */
async function emitArrival(page: Page, contact: string, rowId: number, body: string): Promise<void> {
  await page.evaluate(
    ({ contact, rowId, body }) => {
      if (!window.__skattrEmit) throw new Error("mock emit hook not installed");
      window.__skattrEmit({
        event: "message_received",
        data: {
          contact,
          record: {
            row_id: BigInt(rowId),
            message_id: rowId.toString(16).padStart(32, "0"),
            contact,
            direction: "incoming",
            kind: { kind: "text", body },
            mls_generation: 0n,
            ts_daemon_recv: BigInt(Math.floor(Date.now() / 1000)),
            ts_envelope: BigInt(Math.floor(Date.now() / 1000)),
          },
        },
      });
    },
    { contact, rowId, body },
  );
}

test.describe("live message arrival", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/?fixture=seeded-200-msgs");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    await page.locator(".rail .row").first().click();
    await expect(page.locator(".list")).toBeVisible();

    // First page (50) lands, then up to one more may auto-cascade while the
    // content is shorter than the viewport. Wait for that to settle so the
    // arrival below is the only thing changing the list.
    await expect.poll(() => messageCount(page), { timeout: 5_000 }).toBeGreaterThanOrEqual(50);
    await page.waitForTimeout(1_000);
  });

  test("an arriving message is visible in the open conversation", async ({ page }) => {
    await wheelToBottom(page);
    const before = await messageCount(page);

    await emitArrival(page, PEER_PUBKEY, ARRIVAL_ROW_ID, ARRIVAL_BODY);

    // The store took it — if this fails the defect is above the component.
    await expect.poll(() => messageCount(page), { timeout: 3_000 }).toBe(before + 1);

    const arrival = page.locator(`.bubble[data-row-id="${ARRIVAL_ROW_ID}"]`);
    // Rendered at all: the virtual range must include the new last row.
    await expect(arrival).toBeAttached({ timeout: 3_000 });
    await expect(arrival).toHaveText(new RegExp(ARRIVAL_BODY));
    // And positioned where a human can read it. This is the assertion that
    // distinguishes a real fix from one that only satisfies jsdom.
    await expect(arrival).toBeInViewport({ timeout: 3_000 });
  });

  test("a second arrival is also visible (the append path keeps working)", async ({ page }) => {
    await wheelToBottom(page);

    await emitArrival(page, PEER_PUBKEY, ARRIVAL_ROW_ID, ARRIVAL_BODY);
    await expect(page.locator(`.bubble[data-row-id="${ARRIVAL_ROW_ID}"]`)).toBeInViewport({
      timeout: 3_000,
    });

    // The field report was a message arriving into a conversation that had
    // already received one this session; appending twice is what shifts the
    // previous row out of "last" grouping and changes its height.
    await emitArrival(page, PEER_PUBKEY, ARRIVAL_ROW_ID + 1, "and you");
    const second = page.locator(`.bubble[data-row-id="${ARRIVAL_ROW_ID + 1}"]`);
    await expect(second).toBeAttached({ timeout: 3_000 });
    await expect(second).toBeInViewport({ timeout: 3_000 });
  });
});
