// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { describe, expect, test } from "vitest";
import { render } from "@testing-library/svelte";
import DeliveryIcon from "./DeliveryIcon.svelte";

describe("DeliveryIcon", () => {
  test("renders clock for pending", () => {
    const { container } = render(DeliveryIcon, { props: { status: "pending" } });
    const svg = container.querySelector("svg");
    expect(svg).not.toBeNull();
    expect(container.innerHTML).toContain('<circle cx="12" cy="12" r="10"');
  });

  test("renders check for sent", () => {
    const { container } = render(DeliveryIcon, { props: { status: "sent" } });
    expect(container.innerHTML).toContain('<polyline points="20 6 9 17 4 12"');
  });

  test("renders check-check for delivered", () => {
    const { container } = render(DeliveryIcon, { props: { status: "delivered" } });
    const paths = container.querySelectorAll("svg path");
    expect(paths.length).toBe(2);
  });

  test("renders alert-triangle for failed", () => {
    const { container } = render(DeliveryIcon, { props: { status: "failed" } });
    expect(container.innerHTML).toMatch(/8-14a2 2 0 0 0-3\.48 0/);
  });

  test("title attribute set when provided", () => {
    const { container } = render(DeliveryIcon, {
      props: { status: "delivered", title: "Delivered" },
    });
    expect(container.querySelector("[title='Delivered']")).not.toBeNull();
  });
});
