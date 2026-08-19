// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
import { writable, get } from "svelte/store";

export type AttachmentStatus =
  | "queued"
  | "sending"
  | "receiving"
  | "complete"
  | "failed";

export interface AttachmentState {
  status: AttachmentStatus;
  received: number; // chunks (receiver-side)
  total: number; // chunks
  filename?: string;
  mime?: string;
  size?: number; // bytes (≤100 MiB → JS number is exact)
  available?: boolean; // true once the encrypted-at-rest file can be opened (receiver)
  reason?: string; // when failed
  retrying?: boolean; // re-armed by RetryAttachment, no chunk has moved yet (#144)
}

/**
 * Global, session-scoped live-transfer state keyed by hex attachment_id.
 * Decoupled from the active conversation so events that arrive before a
 * bubble mounts, during a conversation switch, or for a background
 * conversation are all recorded; the bubble reads current state on mount.
 * Cleared on app restart (the deferred restart case — see design §10/§12).
 */
export const attachments = writable<Map<string, AttachmentState>>(new Map());

function patch(aidHex: string, fn: (prev: AttachmentState) => AttachmentState): void {
  attachments.update((m) => {
    const next = new Map(m);
    const prev = next.get(aidHex) ?? { status: "queued" as AttachmentStatus, received: 0, total: 0 };
    next.set(aidHex, fn(prev));
    return next;
  });
}

export function markQueued(
  aidHex: string,
  info: { filename?: string; size?: number; total?: number },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: "queued",
    total: info.total ?? prev.total,
    filename: info.filename ?? prev.filename,
    size: info.size ?? prev.size,
  }));
}

export function applyManifest(
  aidHex: string,
  info: { filename: string; mime: string; size: number; total: number },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    filename: info.filename,
    mime: info.mime,
    size: info.size,
    total: prev.total || info.total,
  }));
}

export function applyProgress(aidHex: string, received: number, total: number): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: prev.status === "complete" ? "complete" : "receiving",
    received,
    total,
    retrying: false,
  }));
}

export function markAvailable(
  aidHex: string,
  info: { filename: string; mime: string; size: number },
): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: "complete",
    received: prev.total || prev.received,
    filename: info.filename,
    mime: info.mime,
    size: info.size,
    available: true,
    retrying: false,
  }));
}

export function applyReceived(
  aidHex: string,
  info: { filename: string; mime: string; size: number },
): void {
  markAvailable(aidHex, info);
}

export function applyFailed(aidHex: string, reason: string): void {
  patch(aidHex, (prev) => ({ ...prev, status: "failed", reason, retrying: false }));
}

/**
 * Move a failed transfer back to waiting after the daemon accepted a retry
 * (#144). The received count is kept: a retry resumes from the chunks already
 * held rather than restarting, so zeroing it would misreport progress. The
 * failure reason is cleared so the bubble stops showing the old error.
 *
 * "queued" rather than "receiving" on purpose — the daemon only re-arms here.
 * The fetch itself starts when the peer is reachable again, which may not be
 * now, and claiming "downloading" before a single chunk has moved would be a
 * lie the user can see. The distinct `retrying` flag is what the bubble reads,
 * so this does not disturb the plain "queued" seed state of a fresh transfer.
 */
export function markRetrying(aidHex: string): void {
  patch(aidHex, (prev) => ({
    ...prev,
    status: "queued",
    reason: undefined,
    retrying: true,
  }));
}

/**
 * Record a completion the daemon already persisted, rather than one observed
 * live. Used by the sender's post-restart rehydration (#176): the transfer
 * store is session-scoped, so a delivered attachment has no state after a
 * restart even though the daemon's `out` row says it completed.
 */
export function markDelivered(aidHex: string): void {
  patch(aidHex, (prev) => ({ ...prev, status: "complete" }));
}

export function attachmentFor(aidHex: string): AttachmentState | undefined {
  return get(attachments).get(aidHex);
}
