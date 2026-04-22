// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Per-peer connection actor.
//!
//! One [`PeerConnection`] task per active peer owns the optional
//! `AuthenticatedConnection<S>`, the pending-ACK map, and the
//! per-peer retry tick. Implementation lands in Task 7.
