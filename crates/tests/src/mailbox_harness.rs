// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Shared test harness for the Phase 2.B mailbox-client integration
//! tests (Tasks 25–29).
//!
//! ## Shape
//!
//! - [`InProcessMailboxFactory`] — a [`TestMailboxFactory`] impl whose
//!   `connect` opens a fresh `tokio::io::duplex` pair, hands the client
//!   half back wrapped in a `MailboxClientHandle`, and pipes the server
//!   half into a shared in-process [`MailboxServer`] via `accept_loop`.
//!   Multiple onions can be registered against the same factory; each
//!   maps to its own `MailboxServer` so we can model a "primary fails
//!   → secondary accepts" scenario for `mailbox_failover`.
//!
//! - [`paired_groups`] — builds a 2-member MLS group between two pools,
//!   returning `(alice_group_id, bob_group_id)`. Mirrors the pattern in
//!   `mls_pair.rs`; pulled into a helper because every mailbox test
//!   needs the MLS pairing.
//!
//! ## What this harness does NOT cover
//!
//! - Full `Daemon::run` orchestration. That requires a stub `TorRuntime`
//!   the test crate does not have today; it lands in Task 32 (real-Tor).
//! - The `PollScheduler` supervisor task. The harness exposes the same
//!   factory but tests that need polling drive `run_one_poll_tick`
//!   directly so they don't have to wait for jittered idle ticks.
//! - Welcome handoff. Tests build the MLS group from scratch in-process,
//!   so both sides know `group_id` from the start.

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use skattr_core::test_exports::{
    MailboxClientHandle, MailboxRepo, MailboxStatus, TestMailboxFactory,
};
use skattr_mailbox::{MailboxServer, Policy, Store};
use std::sync::Mutex as StdMutex;
use tokio::io::DuplexStream;

/// One in-process mailbox server keyed by the onion the integration
/// test uses to route to it. `accept_loop` runs against the
/// server-half of every duplex pair we hand out.
pub struct InProcessMailbox {
    /// Onion string the factory matches against.
    pub onion: String,
    /// Shared mailbox server — its `Store` outlives a single connection
    /// so deposits made on one duplex pair are visible to fetches on
    /// the next one.
    pub server: Arc<MailboxServer>,
    /// Behaviour knob: if `true`, every `connect()` call fails with
    /// `MailboxClientErrorKind::Unreachable` rather than wiring up a
    /// real duplex stream. `mailbox_failover` flips this to model a
    /// dead primary mailbox.
    pub fail_connects: StdMutex<bool>,
}

impl InProcessMailbox {
    /// Build a fresh in-memory server with the recommended policy.
    pub fn new(onion: impl Into<String>) -> Self {
        let store = Arc::new(Store::in_memory().expect("Store::in_memory"));
        let server = Arc::new(MailboxServer::new(store, Policy::recommended()));
        Self {
            onion: onion.into(),
            server,
            fail_connects: StdMutex::new(false),
        }
    }

    /// Cause subsequent `connect()` calls to fail with `Unreachable`.
    /// Used by `mailbox_failover` to simulate a downed primary.
    pub fn fail_next_connects(&self) {
        if let Ok(mut g) = self.fail_connects.lock() {
            *g = true;
        }
    }

    /// Re-enable connects (for symmetry; tests rarely toggle this).
    pub fn allow_connects(&self) {
        if let Ok(mut g) = self.fail_connects.lock() {
            *g = false;
        }
    }
}

/// Multi-mailbox factory. `connect("foo.onion")` returns a fresh client
/// duplex stream paired with an `accept_loop` task running on the
/// matching `InProcessMailbox`'s server.
pub struct InProcessMailboxFactory {
    mailboxes: HashMap<String, Arc<InProcessMailbox>>,
}

impl InProcessMailboxFactory {
    /// Build a factory with a fixed mailbox set keyed by onion.
    pub fn new(boxes: Vec<Arc<InProcessMailbox>>) -> Self {
        let mut mailboxes = HashMap::with_capacity(boxes.len());
        for mb in boxes {
            mailboxes.insert(mb.onion.clone(), mb);
        }
        Self { mailboxes }
    }

    /// Build a factory wrapping a single mailbox.
    pub fn single(mb: Arc<InProcessMailbox>) -> Arc<Self> {
        Arc::new(Self::new(vec![mb]))
    }
}

#[async_trait::async_trait]
impl TestMailboxFactory for InProcessMailboxFactory {
    async fn connect(&self, onion: &str) -> skattr_core::Result<MailboxClientHandle> {
        let mb = self.mailboxes.get(onion).ok_or_else(|| {
            skattr_core::CoreError::MailboxClient(
                skattr_core::error::MailboxClientErrorKind::Unreachable,
            )
        })?;

        // Knob for failover testing.
        let should_fail = mb.fail_connects.lock().map(|g| *g).unwrap_or(false);
        if should_fail {
            return Err(skattr_core::CoreError::MailboxClient(
                skattr_core::error::MailboxClientErrorKind::Unreachable,
            ));
        }

        // Open a fresh duplex pair and hand the server side off to a
        // background `accept_loop` task. The task exits when the client
        // half is dropped.
        let (client_side, server_side): (DuplexStream, DuplexStream) = tokio::io::duplex(64 * 1024);
        let server = mb.server.clone();
        tokio::spawn(async move {
            let _ = server.accept_loop(server_side).await;
        });
        Ok(MailboxClientHandle::from_stream(
            onion.to_string(),
            client_side,
        ))
    }
}

/// Pre-populate a `'mine'` mailbox row pointing at the given onion and
/// flip its status to `Reachable`. Used by tests that exercise the
/// recipient-side polling path.
pub fn seed_mine_mailbox(pool: &skattr_core::test_exports::Pool, onion: &str, now: i64) -> i64 {
    let repo = MailboxRepo::new(pool);
    let id = repo.add_mine(onion, now).unwrap();
    repo.mark_status(id, MailboxStatus::Reachable).unwrap();
    id
}
