// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
import type { PublicKey } from "$lib/ipc/types";

/**
 * Value-equality for `PublicKey`.
 *
 * The ts-rs-generated type is `string`, but the value serialized over IPC is
 * actually a byte array (`number[]`). Comparing two separately-deserialized
 * pubkeys with `===` is therefore reference equality and is ALWAYS false even
 * when the bytes are identical. Every pubkey comparison in the UI must go
 * through this helper.
 */
export function pubkeyEq(
  a: PublicKey | null | undefined,
  b: PublicKey | null | undefined,
): boolean {
  if (a == null || b == null) return a === b;
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    const ab = a as unknown as number[];
    const bb = b as unknown as number[];
    return ab.length === bb.length && ab.every((v, i) => v === bb[i]);
  }
  return String(a) === String(b);
}

/** Canonical string key for a `PublicKey` (Map keys, dedup, `find`, etc.). */
export function pubkeyKey(pk: PublicKey): string {
  return Array.isArray(pk) ? (pk as unknown as number[]).join(",") : String(pk);
}
