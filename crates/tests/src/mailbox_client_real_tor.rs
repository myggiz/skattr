// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! `#[ignore]`-gated real-Tor scenario for Phase 2.B.
//!
//! Run with:
//!
//! ```bash
//! cargo test -p skattr-tests --release -- --ignored mailbox_client_real_tor
//! ```
//!
//! Requirements:
//! - `skattr-mailbox` binary built (`cargo build -p skattr-mailbox --release`).
//! - Arti bootstrap takes approximately 2 minutes on first run.
//! - The test is excluded from CI; trigger it manually to validate a
//!   real-Tor end-to-end scenario before tagging a release.

#[tokio::test]
#[ignore = "requires real Arti circuits + spawned skattr-mailbox binary; manual run only"]
async fn real_tor_offline_delivery_plus_rotation() {
    // TODO Task 32 implementation:
    //
    // 1. Spawn `target/release/skattr-mailbox` via tokio::process::Command
    //    with `--data-dir <tmp>` and `--listen <onion>`. Wait for the UDS
    //    healthcheck at `<tmp>/health.sock` to respond.
    //
    // 2. Spawn two skattr daemons over real onion services using the
    //    pattern from `crates/tests/src/cli_two_daemons.rs`. Each daemon
    //    gets the mailbox onion in its config / via Command::AddMailbox.
    //
    // 3. Drive offline-delivery: bring Bob offline, send Alice→Bob,
    //    bring Bob online, await Event::MessageReceived within 30s.
    //
    // 4. Drive rotation: Alice issues Command::RotateOnion; assert
    //    Bob receives Event::ContactCardReceived with the new version
    //    within one poll cycle.
    //
    // The test is currently a placeholder. The full implementation
    // depends on:
    //   - `Daemon::run` accepting an injected mailbox factory (already
    //     true post-Task 21).
    //   - Task 23.5 for actual HS key rotation (rotate-onion test
    //     today only verifies the version bump, not the address change).
    //   - A test wrapper around `tokio::process::Command::new("skattr-mailbox")`
    //     with healthcheck-poll integration.
    //
    // Until that integration lands, this test is a structural commitment:
    // the symbol exists in the test suite, the `#[ignore]` gate is in
    // place, and the doc string captures the manual-run protocol.
    todo!("implement real-Tor scenario per the comment above");
}
