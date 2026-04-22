// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Send/receive plumbing: outbox queue, retry, dedup, ACK handling.

pub(crate) mod backoff;
pub(crate) mod hub;
pub(crate) mod outbox;
pub(crate) mod peer;
pub(crate) mod receiver;
