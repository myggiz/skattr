// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { describe, it, expect } from "vitest";
import { render } from "@testing-library/svelte";
import ContactRow from "./ContactRow.svelte";
import type { ContactSummary } from "$lib/ipc/types";

const c = (over: Partial<ContactSummary>): ContactSummary =>
  ({
    pubkey: "aa",
    nickname: "Bob",
    onion: "x.onion",
    card_version: 1n,
    added_at: 0n,
    unread_count: 0n,
    last_message_preview: null,
    last_ts_recv: null,
    group_state: "pending_join",
    last_read_row_id: null,
    muted: false,
    peer_mailboxes: [],
    ...over,
  }) as ContactSummary;

describe("ContactRow pending states", () => {
  it("shows 'Not connected yet' for a long-pending contact", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const { getByText } = render(ContactRow, {
      summary: c({ added_at: BigInt(nowSecs - 600) }),
    });
    expect(getByText(/not connected yet/i)).toBeTruthy();
  });

  it("shows 'Connecting…' for a fresh pending contact", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const { getByText } = render(ContactRow, {
      summary: c({ added_at: BigInt(nowSecs) }),
    });
    expect(getByText(/connecting/i)).toBeTruthy();
  });

  it("shows no pending badge for an active contact", () => {
    const { queryByText } = render(ContactRow, {
      summary: c({ group_state: "active" }),
    });
    expect(queryByText(/connecting|not connected/i)).toBeNull();
  });
});
