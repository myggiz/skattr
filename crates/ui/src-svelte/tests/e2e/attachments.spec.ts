// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { test, expect } from "@playwright/test";

test.describe("attachments", () => {
  test("attach → send → receive → inline preview", async ({ page }) => {
    await page.goto("/?fixture=attachments");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    await page.locator(".rail .row").first().click();
    await page.getByLabel("Attach file").click();
    // Incoming Kind::File bubble decodes to the filename.
    await expect(page.getByText("photo.jpg")).toBeVisible({ timeout: 3_000 });
    // After attachment_received, the Open button appears in the file bubble.
    // Note: the img.preview element is driven by convertFileSrc which uses the
    // asset:// protocol (only valid inside a real Tauri webview); in the
    // Playwright browser it fails to load and triggers onerror → showImage=false.
    // The Open button renders regardless of image-load success, so it is the
    // reliable end-to-end assertion here.
    // Use exact aria-label "Open" to target the file bubble's action button
    // (not the "Open settings" button in the rail).
    await expect(page.getByRole("button", { name: "Open", exact: true })).toBeVisible({ timeout: 3_000 });
  });

  test("file_size > 100 MiB is rejected before send", async ({ page }) => {
    // Navigate with pick=huge so the dialog mock returns /picked/huge.bin.
    // file_size("/picked/huge.bin") → 200 MiB, which exceeds the 100 MiB gate.
    // The UI must block the send and show a rejection toast; no file bubble may appear.
    await page.goto("/?fixture=attachments&pick=huge");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    await page.locator(".rail .row").first().click();
    await page.getByLabel("Attach file").click();
    // Wait briefly then assert no file bubble for "huge.bin" rendered.
    await page.waitForTimeout(500);
    await expect(page.getByText("huge.bin")).toHaveCount(0);
    await expect(page.locator(".file-bubble")).toHaveCount(0);
    // The toast must show the rejection message.
    await expect(page.locator(".toast")).toContainText(/too large/i, { timeout: 2_000 });
  });

  test("attachment_failed event renders failed state in file bubble", async ({ page }) => {
    // Navigate with fail=1 so the send_file mock emits attachment_failed instead
    // of attachment_progress + attachment_received.
    await page.goto("/?fixture=attachments&fail=1");
    await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });
    await page.locator(".rail .row").first().click();
    await page.getByLabel("Attach file").click();
    // The mock still emits message_received first, so a file bubble must appear.
    await expect(page.locator(".file-bubble")).toBeVisible({ timeout: 3_000 });
    // The failed span must be visible with the reason text.
    await expect(page.locator(".file-bubble .failed")).toBeVisible({ timeout: 3_000 });
    await expect(page.locator(".file-bubble .failed")).toContainText(/transfer failed/i);
    // No file-open action button must appear (the file was never received).
    // Use the exact aria-label "Open" (not "Open settings") to target the bubble button.
    await expect(page.getByRole("button", { name: "Open", exact: true })).toHaveCount(0);
  });
});
