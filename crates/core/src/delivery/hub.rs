// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Daemon-scoped delivery router.
//!
//! [`DeliveryHub`] maps `PublicKey → mpsc::Sender<DeliveryJob>`,
//! spawning a [`crate::delivery::peer::PeerConnection`] actor on the
//! first send to a peer, routing inbound post-handshake connections
//! into the same actors via `ingest`, and running a periodic
//! `seen_messages` sweep. Implementation lands in Task 8.
