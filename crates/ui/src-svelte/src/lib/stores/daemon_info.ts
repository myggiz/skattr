// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { writable } from "svelte/store";
import type { CommandResult } from "$lib/ipc/types";

// CommandResult shape from ts-rs: { result: "daemon_info", data: { ... } }
export type DaemonInfoResult = Extract<CommandResult, { result: "daemon_info" }>;

export const daemonInfo = writable<DaemonInfoResult | null>(null);
