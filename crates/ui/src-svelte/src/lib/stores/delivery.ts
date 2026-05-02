// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable, get } from "svelte/store";
import type { DeliveryStatus, Hex16 } from "$lib/ipc/types";

/**
 * Map keyed by lowercase-hex `message_id` (Hex16 stringified).
 * Updated by Event::DeliveryStatusChanged (subscribed elsewhere)
 * and by send.ts when MessageSent reply arrives (sets
 * Queued or Delivered immediately).
 *
 * ts-rs emits DeliveryStatus as:
 *   "Queued" | "Delivered" | "Deposited" | { "Failed": string }
 * ts-rs emits Hex16 as:
 *   string
 */
export const delivery = writable<Map<string, DeliveryStatus>>(new Map());

export function recordDeliveryStatus(
  messageHex: string,
  status: DeliveryStatus,
): void {
  delivery.update((m) => {
    const next = new Map(m);
    next.set(messageHex, status);
    return next;
  });
}

export function statusForMessageHex(messageHex: string): DeliveryStatus | undefined {
  return get(delivery).get(messageHex);
}

/**
 * Convert a Hex16 (ts-rs emits as `string`) to a lowercase hex string.
 * Handles both the plain-string form and, defensively, the tuple-struct
 * (`{ "0": number[] }`) form in case the emission shape ever changes.
 */
export function hex16ToString(h: Hex16): string {
  if (typeof h === "string") return h.toLowerCase();
  // Defensive fallback for tuple-struct shape: { "0": number[] }
  const bytes = (h as unknown as { "0": number[] })["0"];
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Icon-status string for a wire DeliveryStatus.
 *
 * Maps the four wire states to the four icon strings consumed by
 * DeliveryIcon. ts-rs emits the enum as:
 *   "Queued" | "Delivered" | "Deposited" | { "Failed": string }
 */
export function deliveryToIconStatus(
  s: DeliveryStatus | undefined,
): "pending" | "sent" | "delivered" | "failed" {
  if (!s) return "pending";
  if (s === "Queued") return "pending";
  if (s === "Delivered") return "delivered";
  if (s === "Deposited") return "sent";
  if (typeof s === "object" && "Failed" in s) return "failed";
  return "pending";
}
