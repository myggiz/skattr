// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { readable, type Readable } from "svelte/store";

/** Unix seconds, updated every 30 s. Lets pending contacts re-evaluate their
 *  display state (Connecting… → Not connected yet) as time passes, without
 *  needing a daemon event. */
export const now: Readable<number> = readable(
  Math.floor(Date.now() / 1000),
  (set) => {
    const id = setInterval(() => set(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(id);
  },
);
