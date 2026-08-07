// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { test, expect } from "@playwright/test";

test("generate invite happy path", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=invite-flow");
  // Wait for the shell to load.
  await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });

  // Click the "Generate invite" rail button to open the dialog.
  await page.getByRole("button", { name: "Generate invite" }).click();

  // The dialog form step renders with heading "Generate invite".
  await expect(page.getByRole("heading", { name: "Generate invite" })).toBeVisible();

  // Click the "Generate" submit button (exact match to avoid matching the rail button).
  await page.getByRole("button", { name: "Generate", exact: true }).click();

  // Result step: the invite URL appears in a <code> element.
  await expect(page.locator("code.url")).toContainText("skattr://invite/v1#fixture");

  // The QR SVG rendered by the mock should be injected into the .qr div.
  await expect(page.locator(".qr svg")).toBeVisible();

  // Close the dialog.
  await page.getByRole("button", { name: "Done" }).click();

  // Dialog is gone; shell is still visible.
  await expect(page.getByRole("heading", { name: "Generate invite" })).not.toBeVisible();
});
