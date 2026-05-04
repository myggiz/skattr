// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

/**
 * deepLinkInviteUrl: set by +layout.svelte when a skattr://invite/v1#…
 * deep-link event arrives from the Tauri host. +page.svelte subscribes to
 * this store and opens the AddContactDialog with the URL pre-filled, then
 * clears the store.
 */
import { writable } from "svelte/store";

export const deepLinkInviteUrl = writable<string | null>(null);
