// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.
// Human-readable error messages for structured IPC errors.

import type { IpcError, DaemonErrorKind } from "./types";

function daemonKindMessage(k: DaemonErrorKind): string {
  switch (k.kind) {
    case "invite_expired": return "This invite link has expired.";
    case "invite_consumed": return "This invite link has already been used.";
    case "invite_signature_invalid":
      return "This invite couldn't be verified — it may be corrupted or tampered with.";
    case "contact_not_found": return "Contact not found.";
    case "contact_ambiguous": return "That name matches more than one contact.";
    case "delivery_timeout": return "Couldn't reach your contact — they may be offline.";
    case "tor_not_ready": return "Still connecting to Tor — try again in a moment.";
    case "group_corrupt": return "This conversation's secure state is damaged.";
    case "storage_error": return "A local storage error occurred.";
    case "search_syntax": return "That search query isn't valid.";
    case "invalid_argument": return k.data.message;
    case "unauthorized": return "Not authorized.";
    default: return "Something went wrong.";
  }
}

/** Human-readable message for a structured IPC error. Never surfaces a raw
 *  internal string as the primary message. */
export function errorMessage(err: IpcError): string {
  switch (err.err) {
    case "daemon": return daemonKindMessage(err.data);
    case "vault_not_ready": return "The app is still starting — try again in a moment.";
    case "auth_denied": return "Not authorized.";
    case "unknown_command": return "This action isn't available.";
    case "frame_too_large": return "That request was too large.";
    case "codec":
    case "internal":
    default: return "Something went wrong.";
  }
}
