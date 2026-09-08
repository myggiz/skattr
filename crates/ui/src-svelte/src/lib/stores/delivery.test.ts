// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach } from "vitest";
import { get } from "svelte/store";
import {
  delivery,
  recordDeliveryStatus,
  statusForMessageHex,
  hex16ToString,
  deliveryToIconStatus,
  deliveryStateFromRecord,
} from "./delivery";
import type { DeliveryStatus } from "$lib/ipc/types";

describe("delivery store", () => {
  beforeEach(() => {
    delivery.set(new Map());
  });

  test("recordDeliveryStatus sets the entry", () => {
    const status: DeliveryStatus = "Queued";
    recordDeliveryStatus("aabbccdd", status);
    const map = get(delivery);
    expect(map.get("aabbccdd")).toEqual(status);
  });

  test("statusForMessageHex returns undefined for missing key", () => {
    expect(statusForMessageHex("ffeeddcc")).toBeUndefined();
  });

  test("recordDeliveryStatus overwrites prior entry", () => {
    const queued: DeliveryStatus = "Queued";
    const delivered: DeliveryStatus = "Delivered";
    recordDeliveryStatus("aabb", queued);
    recordDeliveryStatus("aabb", delivered);
    expect(get(delivery).get("aabb")).toEqual(delivered);
  });
});

describe("deliveryToIconStatus", () => {
  test("undefined → pending", () => {
    expect(deliveryToIconStatus(undefined)).toBe("pending");
  });
  test("Queued → pending", () => {
    expect(deliveryToIconStatus("Queued")).toBe("pending");
  });
  test("Delivered → delivered", () => {
    expect(deliveryToIconStatus("Delivered")).toBe("delivered");
  });
  test("Deposited → sent", () => {
    expect(deliveryToIconStatus("Deposited")).toBe("sent");
  });
  test("Failed → failed", () => {
    expect(deliveryToIconStatus({ Failed: "boom" })).toBe("failed");
  });
});

describe("deliveryStateFromRecord", () => {
  test("dismissed_at wins over a failed_reason set at the same time", () => {
    expect(
      deliveryStateFromRecord({
        delivered_at: null,
        dismissed_at: 1700n,
        failed_reason: "Not delivered — this contact has no mailbox.",
      }),
    ).toBe("dismissed");
  });

  test("delivered_at wins over everything else", () => {
    expect(
      deliveryStateFromRecord({
        delivered_at: 1700n,
        dismissed_at: 1701n,
        failed_reason: "stale",
      }),
    ).toBe("delivered");
  });

  test("failed_reason alone -> failed", () => {
    expect(
      deliveryStateFromRecord({ delivered_at: null, dismissed_at: null, failed_reason: "boom" }),
    ).toBe("failed");
  });

  test("none set -> pending", () => {
    expect(
      deliveryStateFromRecord({ delivered_at: null, dismissed_at: null, failed_reason: null }),
    ).toBe("pending");
  });
});

describe("hex16ToString", () => {
  test("string passthrough", () => {
    expect(hex16ToString("aabbccdd00112233445566778899aabb")).toBe(
      "aabbccdd00112233445566778899aabb",
    );
  });
  test("normalises to lowercase", () => {
    expect(hex16ToString("AABBCCDD")).toBe("aabbccdd");
  });
});
