// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Integration test stub: Noise_XK handshake round-trip.
//!
//! Implemented in Phase 1 workstream 1.B — kept here as a marker so the
//! test harness wires up early.

#[test]
fn handshake_stub_compiles() {
    // Placeholder: replaced by a real integration test once
    // transport::noise has a non-todo!() implementation.
    let _ = skattr_core::error::CoreError::Transport("noop".into());
}
