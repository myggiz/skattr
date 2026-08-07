// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

import { writable, type Readable } from "svelte/store";

export interface ToastMessage {
  message: string;
  /** Monotonic id; the latest is shown. */
  id: number;
}

const internal = writable<ToastMessage | null>(null);
let counter = 0;
let timer: ReturnType<typeof setTimeout> | null = null;

function show(message: string, durationMs = 1500): void {
  if (timer) clearTimeout(timer);
  counter += 1;
  internal.set({ message, id: counter });
  timer = setTimeout(() => {
    internal.set(null);
    timer = null;
  }, durationMs);
}

function clear(): void {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  internal.set(null);
}

export const currentToast: Readable<ToastMessage | null> = { subscribe: internal.subscribe };
export const toast = { show, clear };
