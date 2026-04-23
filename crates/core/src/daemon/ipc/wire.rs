// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! IPC wire types: `IpcRequest`, `IpcResponse`, `IpcError`,
//! `EventFilter`. Every variant round-trips through CBOR.

use serde::{Deserialize, Serialize};

use crate::daemon::commands::{Command, CommandResult};
use crate::daemon::events::Event;
use crate::daemon::DaemonErrorKind;
use crate::identity::PublicKey;

/// Maximum CBOR body size accepted on the IPC socket. 1 MiB.
///
/// Envelopes larger than this still go via the MLS + DeliveryHub
/// paths — the IPC wire is for control plane and small payloads.
pub const MAX_IPC_BODY: u32 = 1024 * 1024;

/// Subscription filter for `IpcRequest::Subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "filter", content = "data", rename_all = "snake_case")]
pub enum EventFilter {
    /// Forward every event the daemon emits.
    All,
    /// Only `MessageReceived` + `DeliveryStatusChanged` relating to this peer.
    Contact(PublicKey),
    /// Only `TorStatusChanged`.
    TorStatus,
}

/// Request frame sent from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "req", content = "data", rename_all = "snake_case")]
pub enum IpcRequest {
    /// One-shot command; expect a single `Ok` or `Err` response.
    Execute(Command),
    /// Long-lived subscription; expect one `Ok(Subscribed)` then a
    /// stream of `Event(..)` frames until the client hangs up.
    Subscribe(EventFilter),
    /// Graceful daemon shutdown. Expect `Ok(ShuttingDown)` then `Bye`.
    Shutdown,
}

/// Response frame sent from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", content = "data", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Successful command result.
    Ok(CommandResult),
    /// Typed error.
    Err(IpcError),
    /// Event delivery (only after a `Subscribe` request).
    Event(Event),
    /// Terminal frame before the server closes the connection.
    Bye,
}

/// Stable wire error. `Daemon(DaemonErrorKind)` carries the library-error
/// projection; `Internal(String)` is a 256-byte-truncated fallback whose
/// full detail stays server-side in tracing logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "err", content = "data", rename_all = "snake_case")]
pub enum IpcError {
    /// `SO_PEERCRED`/`getpeereid` reported a UID other than the daemon's.
    AuthDenied,
    /// CBOR decode failure (connection stays open).
    Codec(String),
    /// Declared frame body exceeded `MAX_IPC_BODY`.
    FrameTooLarge {
        /// Reported body length.
        got: u32,
        /// Maximum accepted body length.
        max: u32,
    },
    /// Variant is reserved for a future phase (e.g. `CreateGroup` in 1.F).
    UnknownCommand,
    /// Daemon is still booting; retry.
    VaultNotReady,
    /// Typed library error from the daemon.
    Daemon(DaemonErrorKind),
    /// Unmapped failure. Body truncated to 256 bytes.
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::commands::{Command, CommandResult};
    use crate::daemon::DaemonErrorKind;
    use crate::identity::PublicKey;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(value, &mut buf).unwrap();
        ciborium::de::from_reader(&buf[..]).unwrap()
    }

    #[test]
    fn ipc_request_variants_roundtrip() {
        let reqs: Vec<IpcRequest> = vec![
            IpcRequest::Execute(Command::ListContacts),
            IpcRequest::Subscribe(EventFilter::All),
            IpcRequest::Subscribe(EventFilter::Contact(PublicKey([3; 32]))),
            IpcRequest::Subscribe(EventFilter::TorStatus),
            IpcRequest::Shutdown,
        ];
        for r in &reqs {
            let _back: IpcRequest = roundtrip(r);
        }
    }

    #[test]
    fn ipc_response_variants_roundtrip() {
        let resps: Vec<IpcResponse> = vec![
            IpcResponse::Ok(CommandResult::Ok),
            IpcResponse::Err(IpcError::AuthDenied),
            IpcResponse::Err(IpcError::Codec("bad cbor".into())),
            IpcResponse::Err(IpcError::FrameTooLarge { got: 2_000_000, max: 1_048_576 }),
            IpcResponse::Err(IpcError::UnknownCommand),
            IpcResponse::Err(IpcError::VaultNotReady),
            IpcResponse::Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound)),
            IpcResponse::Err(IpcError::Internal("whatever".into())),
            IpcResponse::Bye,
        ];
        for r in &resps {
            let _back: IpcResponse = roundtrip(r);
        }
    }
}
