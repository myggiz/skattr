// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { describe, it, expect, vi, afterEach } from "vitest";
import { render } from "@testing-library/svelte";
import Toast from "./Toast.svelte";
import { toast, currentToast } from "../stores/toast";
import { get } from "svelte/store";

describe("Toast", () => {
  afterEach(() => {
    vi.useRealTimers();
    toast.clear();
  });

  it("renders the current toast message", () => {
    toast.show("Copied");
    const { getByText } = render(Toast);
    expect(getByText("Copied")).toBeTruthy();
  });

  it("auto-dismisses after 1500 ms", () => {
    vi.useFakeTimers();
    toast.show("hi");
    expect(get(currentToast)).not.toBeNull();
    vi.advanceTimersByTime(1500);
    expect(get(currentToast)).toBeNull();
  });

  it("replaces an in-flight toast with a new one", () => {
    toast.show("first");
    toast.show("second");
    expect(get(currentToast)?.message).toBe("second");
  });
});
