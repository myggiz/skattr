// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { TorStatus } from "$lib/ipc/types";

export const torStatus = writable<TorStatus | null>(null);
