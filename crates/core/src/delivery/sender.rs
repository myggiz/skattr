// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Drives the outbox: pick next entry, choose direct vs mailbox path,
//! attempt delivery, record outcome.

use crate::error::Result;

/// Background sender task handle.
pub struct Sender {
    _private: (),
}

impl Sender {
    /// Run the sender loop. Returns when cancellation is signalled.
    pub async fn run(&self) -> Result<()> {
        todo!("loop: outbox.due → dial or deposit → ack/reschedule")
    }
}
