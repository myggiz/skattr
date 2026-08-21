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

// #173: the sidebar is persistent chrome — visible on every conversation,
// for every contact at once. Message content and per-contact activity timing
// must not be readable there.
describe("ContactRow privacy (#173)", () => {
  it("never renders message content, even when a preview is present", () => {
    const { container } = render(ContactRow, {
      summary: c({
        group_state: "active",
        last_message_preview: "meet me at the usual place",
      }),
    });
    expect(container.textContent).not.toContain("meet me at the usual place");
  });

  it("never renders a relative timestamp", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    const { container } = render(ContactRow, {
      summary: c({
        group_state: "active",
        last_ts_recv: BigInt(nowSecs - 240), // would have rendered "4m"
      }),
    });
    // Any of the old relativeTs() shapes: 12s / 4m / 2h.
    expect(container.textContent).not.toMatch(/\b\d+[smh]\b/);
  });

  it("shows unread as a dot carrying no count", () => {
    const { container } = render(ContactRow, {
      summary: c({ group_state: "active", unread_count: 7n }),
    });
    const dot = container.querySelector(".unread-dot");
    expect(dot).not.toBeNull();
    // "7 unread" is itself an activity signal — the row says only "something new".
    expect(container.textContent).not.toMatch(/\d/);
  });

  it("shows no unread affordance when nothing is unread", () => {
    const { container } = render(ContactRow, {
      summary: c({ group_state: "active", unread_count: 0n }),
    });
    const dot = container.querySelector(".unread-dot.on");
    expect(dot).toBeNull();
  });
});

describe("ContactRow established styling (#173, #195)", () => {
  it("marks an active contact as established", () => {
    const { container } = render(ContactRow, {
      summary: c({ group_state: "active" }),
    });
    expect(container.querySelector(".title.established")).not.toBeNull();
  });

  it("does not mark a pending contact as established", () => {
    const { container } = render(ContactRow, {
      summary: c({ group_state: "pending_join" }),
    });
    expect(container.querySelector(".title.established")).toBeNull();
  });

  it("does not mark a corrupt contact as established", () => {
    // Reusing pendingState() === null would have wrongly painted this green.
    const { container } = render(ContactRow, {
      summary: c({ group_state: "corrupt" }),
    });
    expect(container.querySelector(".title.established")).toBeNull();
  });

  it("never renders a row as both pending and connected", () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    for (const s of ["active", "pending_join", "corrupt"] as const) {
      const { container } = render(ContactRow, {
        summary: c({ group_state: s, added_at: BigInt(nowSecs - 600) }),
      });
      const both =
        container.querySelector(".row.pending") !== null &&
        container.querySelector(".title.established") !== null;
      expect(both).toBe(false);
    }
  });
});
