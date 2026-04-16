// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Mailbox listener: accept streams, dispatch DEPOSIT / CHALLENGE /
//! FETCH / DELETE, reply.

use anyhow::Result;

use crate::config::MailboxConfig;

/// Run the server until signalled to stop.
pub async fn run(_config: MailboxConfig) -> Result<()> {
    // TODO(phase-2): open Store, bootstrap Arti, publish onion, loop
    //                accept → dispatch → respond. This scaffold stays
    //                compilation-clean by returning immediately.
    tracing::warn!("mailbox server not yet implemented (Phase 2 work)");
    Ok(())
}
