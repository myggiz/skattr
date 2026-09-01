// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// Consecutive messages from one side within a short window read as a single
// utterance, so they are stacked tightly instead of each carrying a full
// bubble gap.

/** Seconds within which two same-side messages still read as one turn. */
export const GROUP_WINDOW_SECS = 300;

type Groupable = {
  direction: string;
  ts_daemon_recv: bigint;
};

/** Does `current` continue the group started by `previous`? */
export function continuesGroup(
  previous: Groupable | null,
  current: Groupable,
): boolean {
  if (previous === null) return false;
  if (previous.direction !== current.direction) return false;
  const delta = current.ts_daemon_recv - previous.ts_daemon_recv;
  // Timestamps are display-only (ordering comes from MLS generation), so a
  // negative delta means skew, not a tight group.
  return delta >= 0n && delta <= BigInt(GROUP_WINDOW_SECS);
}
