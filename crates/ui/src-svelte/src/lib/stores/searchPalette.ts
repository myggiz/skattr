// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import { writable } from "svelte/store";
import type { SearchHitRecord } from "$lib/ipc/types";

interface State {
  open: boolean;
  query: string;
  results: SearchHitRecord[];
  loading: boolean;
}

const _state = writable<State>({ open: false, query: "", results: [], loading: false });

export const searchPalette = _state;

export function openPalette(): void {
  _state.update((s) => ({ ...s, open: true }));
}

export function closePalette(): void {
  _state.update((s) => ({ ...s, open: false, query: "", results: [], loading: false }));
}

export function setQuery(q: string): void {
  _state.update((s) => ({ ...s, query: q }));
}

export function setResults(results: SearchHitRecord[]): void {
  _state.update((s) => ({ ...s, results, loading: false }));
}

export function setLoading(loading: boolean): void {
  _state.update((s) => ({ ...s, loading }));
}

/**
 * focusedRowId: set by SearchPalette when the user picks a result.
 * VirtualMessageList watches this and scrolls the row into view,
 * then clears it after the highlight animation (1200 ms).
 * Stored as bigint | null to match MessageRecord.row_id's type.
 */
export const focusedRowId = writable<bigint | null>(null);

export function setFocusedRowId(id: bigint | null): void {
  focusedRowId.set(id);
}
