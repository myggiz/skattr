// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! CLI ↔ daemon IPC transport.
//!
//! Cross-platform aliases:
//!   - `IpcStream`       — the client-side stream type for IPC connections.
//!   - `PeerId`          — opaque "this user" identity for peer auth.
//!   - `ENDPOINT_FILENAME` — relative file under `data_dir` the daemon binds (Unix) or writes the pipe name to (Windows).

pub mod client;
pub mod codec;
pub mod server;
pub mod wire;

pub use client::{IpcClient, IpcClientError};

/// Filename (relative to `data_dir`) of the daemon's IPC endpoint.
/// On Unix this is the AF_UNIX socket file; on Windows it is the
/// discovery file containing the named-pipe name.
#[cfg(unix)]
pub const ENDPOINT_FILENAME: &str = "ipc.sock";
/// Discovery filename used by the Windows IPC client.
#[cfg(target_os = "windows")]
pub const ENDPOINT_FILENAME: &str = "ipc.endpoint";

/// Client-side IPC stream type. Selected at compile time per platform.
#[cfg(unix)]
pub type IpcStream = tokio::net::UnixStream;
/// Windows pipe-client stream type.
#[cfg(target_os = "windows")]
pub type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Opaque "this user" identity for the daemon's peer-auth allow-list.
/// Unix: numeric uid. Windows: raw SID bytes (variable length).
#[cfg(unix)]
pub type PeerId = u32;
/// Windows SID bytes of the daemon's user.
#[cfg(target_os = "windows")]
pub type PeerId = Vec<u8>;

/// Return the daemon's own `PeerId`. Platform-conditional: Unix returns
/// the process's effective uid; Windows returns the user SID bytes.
#[cfg(unix)]
pub fn current_peer_id() -> PeerId {
    server::current_uid()
}
/// Return the daemon's own user SID on Windows.
#[cfg(target_os = "windows")]
pub fn current_peer_id() -> PeerId {
    server::current_sid()
}
