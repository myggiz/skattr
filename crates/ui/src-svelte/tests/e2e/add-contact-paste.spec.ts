// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { test, expect } from "@playwright/test";

test("add contact via paste", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=add-contact-flow");
  // Wait for the shell to load.
  await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });

  // Click "+ Add" button in the rail header.
  await page.getByRole("button", { name: "+ Add" }).click();

  // The dialog opens with heading "New contact".
  await expect(page.getByRole("heading", { name: "New contact" })).toBeVisible();

  // Paste-tab textarea accepts the invite URL.
  await page.getByPlaceholder(/skattr:\/\/invite/i).fill("skattr://invite/v1#test");

  // "Add contact" button submits the form.
  await page.getByRole("button", { name: "Add contact", exact: true }).click();

  // After success the dialog closes (refreshContacts + onClose).
  await expect(page.getByRole("heading", { name: "New contact" })).not.toBeVisible();
});
