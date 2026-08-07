// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/svelte";
import ConfirmDialog from "./ConfirmDialog.svelte";

describe("ConfirmDialog", () => {
  it("renders title and body", () => {
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Archive Bob?",
        body: "Bob disappears from your contacts.",
        confirmLabel: "Archive",
        onConfirm: vi.fn(),
        onCancel: vi.fn(),
      },
    });
    expect(getByText("Archive Bob?")).toBeTruthy();
    expect(getByText("Bob disappears from your contacts.")).toBeTruthy();
  });

  it("calls onConfirm when confirm button clicked", async () => {
    const onConfirm = vi.fn().mockResolvedValue(undefined);
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        onConfirm,
        onCancel: vi.fn(),
      },
    });
    await fireEvent.click(getByText("Archive"));
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("calls onCancel when cancel button clicked", async () => {
    const onCancel = vi.fn();
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        onConfirm: vi.fn(),
        onCancel,
      },
    });
    await fireEvent.click(getByText("Cancel"));
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("uses --danger styling when danger=true", () => {
    const { getByText } = render(ConfirmDialog, {
      props: {
        title: "Title",
        body: "Body",
        confirmLabel: "Archive",
        danger: true,
        onConfirm: vi.fn(),
        onCancel: vi.fn(),
      },
    });
    const btn = getByText("Archive") as HTMLButtonElement;
    expect(btn.classList.contains("danger")).toBe(true);
  });
});
