// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
//
// @vitest-environment node

import { expect, test } from "vitest";
import { continuesGroup, GROUP_WINDOW_SECS } from "./messageGrouping";

const msg = (direction: "incoming" | "outgoing", ts: number) => ({
  direction,
  ts_daemon_recv: BigInt(ts),
  kind: { kind: "text" },
});

const file = (direction: "incoming" | "outgoing", ts: number) => ({
  direction,
  ts_daemon_recv: BigInt(ts),
  kind: { kind: "file" },
});

test("consecutive messages from the same side, close in time, are one group", () => {
  expect(continuesGroup(msg("incoming", 1000), msg("incoming", 1010))).toBe(true);
});

test("a reply from the other side breaks the group", () => {
  expect(continuesGroup(msg("incoming", 1000), msg("outgoing", 1010))).toBe(false);
});

test("a long pause breaks the group", () => {
  // Two messages an hour apart are not one utterance, however they are stacked.
  const later = 1000 + GROUP_WINDOW_SECS + 1;
  expect(continuesGroup(msg("incoming", 1000), msg("incoming", later))).toBe(false);
});

test("the boundary itself still groups", () => {
  const edge = 1000 + GROUP_WINDOW_SECS;
  expect(continuesGroup(msg("incoming", 1000), msg("incoming", edge))).toBe(true);
});

test("an attachment does not start or continue a group", () => {
  // The file bubble is a card with its own silhouette and never receives the
  // grouped prop, so grouping across one leaves the text bubble tailless
  // beneath a card that kept its tail.
  expect(continuesGroup(file("incoming", 1000), msg("incoming", 1010))).toBe(false);
  expect(continuesGroup(msg("incoming", 1000), file("incoming", 1010))).toBe(false);
  expect(continuesGroup(file("incoming", 1000), file("incoming", 1010))).toBe(false);
});

test("no previous message means no group", () => {
  expect(continuesGroup(null, msg("incoming", 1000))).toBe(false);
});

test("clock skew does not silently group", () => {
  // ts is display-only and daemon-assigned; an out-of-order pair must not be
  // treated as a tight group just because the difference is negative.
  expect(continuesGroup(msg("incoming", 5000), msg("incoming", 1000))).toBe(false);
});
