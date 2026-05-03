// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

import { test, expect } from "@playwright/test";

test("contact details: short hash, rename, archive", async ({ page }) => {
  await page.goto("/?vault=yes&fixture=seeded-contact");
  // Wait for the main shell to render.
  await expect(page.locator(".shell")).toBeVisible({ timeout: 10_000 });

  // The seeded contact row chevron has aria-label="Contact details".
  await page.getByRole("button", { name: "Contact details" }).first().click();

  // The ContactDetailsPanel is now visible.
  await expect(page.locator(".panel")).toBeVisible();

  // Pubkey short-hash: ab×32 hex → "abababab…abababab"
  await expect(page.locator(".panel .value.mono").first()).toContainText("abababab…abababab");

  // Rename: clear the input and type a new nickname, then Save.
  const nicknameInput = page.getByLabel("Nickname");
  await nicknameInput.fill("Renamed");
  await page.getByRole("button", { name: "Save" }).click();

  // Archive flow: click "Archive" in the panel to open the confirm dialog.
  await page.getByRole("button", { name: "Archive" }).first().click();

  // ConfirmDialog should be visible with an "Archive …?" heading.
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.locator("[role='dialog'] h2")).toContainText("Archive");

  // Confirm the archive by clicking the danger "Archive" button inside the modal.
  // The panel's own Archive button is hidden ({#if !confirmOpen}), so only one
  // "Archive" button remains: the modal's confirm button.
  await page.getByRole("button", { name: "Archive" }).click();
});
