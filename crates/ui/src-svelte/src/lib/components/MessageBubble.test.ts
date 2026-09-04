// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, expect, test, beforeEach } from "vitest";
import { render, screen } from "@testing-library/svelte";
import MessageBubble from "./MessageBubble.svelte";
import { delivery, recordDeliveryStatus } from "$lib/stores/delivery";
import type { MessageRecord } from "$lib/ipc/types";

function makeOutgoing(overrides: Partial<MessageRecord> = {}): MessageRecord {
  return {
    row_id: 1n,
    message_id: "ab".repeat(16),
    contact: "cd".repeat(32),
    direction: "outgoing",
    kind: { kind: "text", body: "hello" },
    mls_generation: 1n,
    ts_daemon_recv: 1_700_000_000n,
    ts_envelope: 1_700_000_000n,
    delivered_at: null,
    dismissed_at: null,
    failed_reason: null,
    ...overrides,
  };
}

beforeEach(() => {
  delivery.set(new Map());
});

describe("MessageBubble failed/dismissed state", () => {
  test("renders the failure reason and both actions on a failed message", () => {
    const rec = makeOutgoing({ failed_reason: "Not delivered — no mailbox." });
    render(MessageBubble, { props: { record: rec, grouped: false } });

    expect(screen.getByText(/no mailbox/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /resend/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /dismiss/i })).toBeTruthy();
  });

  // A live DeliveryStatusChanged{Failed} event lands in the delivery map
  // (recordDeliveryStatus) before the conversation is ever reloaded with
  // failed_reason set on the record — this is the exact gap the bug report
  // named: the record alone still has failed_reason: null.
  test("a live Failed event (no record.failed_reason yet) renders the reason and both actions", () => {
    const rec = makeOutgoing({ failed_reason: null });
    recordDeliveryStatus("ab".repeat(16), { Failed: "Not delivered — no mailbox." });
    render(MessageBubble, { props: { record: rec, grouped: false } });

    expect(screen.getByText(/no mailbox/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /resend/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /dismiss/i })).toBeTruthy();
  });

  test("a dismissed message keeps its text but offers no actions", () => {
    const rec = makeOutgoing({
      failed_reason: "Not delivered — no mailbox.",
      dismissed_at: 1700n,
    });
    render(MessageBubble, { props: { record: rec, grouped: false } });

    expect(screen.getByText("hello")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /resend/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /dismiss/i })).toBeNull();
  });

  test("a dismissed message shows no actions even when the delivery map still carries Failed", () => {
    const rec = makeOutgoing({ failed_reason: null, dismissed_at: 1700n });
    recordDeliveryStatus("ab".repeat(16), { Failed: "Not delivered — no mailbox." });
    render(MessageBubble, { props: { record: rec, grouped: false } });

    expect(screen.getByText("hello")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /resend/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /dismiss/i })).toBeNull();
  });
});
