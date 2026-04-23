// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! CLI ↔ daemon IPC transport.

pub mod client;
pub mod codec;
pub mod server;
pub mod wire;

pub use client::{IpcClient, IpcClientError};
