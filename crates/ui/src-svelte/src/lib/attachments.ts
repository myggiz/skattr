// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { invoke } from "@tauri-apps/api/core";
import type { Kind } from "$lib/ipc/types";

/** Daemon hard cap (100 MiB) — > this is blocked pre-send. */
export const MANIFEST_SIZE_HARD = 100 * 1024 * 1024;
/** Offline-lane cap (10 MiB) — 10–100 MiB is soft-warned. */
export const MANIFEST_SIZE_SOFT = 10 * 1024 * 1024;

export interface ManifestSummary {
  attachment_id: string; // hex, matches hex16ToString keys
  filename: string;
  mime: string;
  total_size: number;
}

/** Human-readable binary size. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}

export function isImage(mime: string | undefined): boolean {
  return typeof mime === "string" && mime.startsWith("image/");
}

export function mimeIconName(mime: string | undefined): "image" | "file" {
  return isImage(mime) ? "image" : "file";
}

/**
 * Decode a `Kind::File` manifest via the canonical Rust shell command.
 *
 * `manifest` is declared `string` by ts-rs but is a runtime number[] (the
 * serde_json serialization of the core `Vec<u8>` field). We pass it through
 * untouched; the Rust command param is `Vec<u8>`. Never base64-decode here.
 */
export async function decodeManifest(
  fileKind: Extract<Kind, { kind: "file" }>,
): Promise<ManifestSummary> {
  const manifest = fileKind.manifest as unknown as number[];
  return await invoke<ManifestSummary>("decode_attachment_manifest", { manifest });
}

const _memo = new Map<string, Promise<ManifestSummary>>();

/** Decode-once-per-message-id memo (avoids re-decoding on every re-render). */
export function decodeManifestMemo(
  messageIdHex: string,
  fileKind: Extract<Kind, { kind: "file" }>,
): Promise<ManifestSummary> {
  const hit = _memo.get(messageIdHex);
  if (hit) return hit;
  const p = decodeManifest(fileKind);
  _memo.set(messageIdHex, p);
  // On rejection, drop from memo so a later mount can retry.
  p.catch(() => _memo.delete(messageIdHex));
  return p;
}
