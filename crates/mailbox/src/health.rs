// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Local-only Unix-domain healthcheck server.
//!
//! Bound at `${data_dir}/health.sock`, mode 0600. One-line
//! line-based protocol: `GET /health\n` → `ok\n` or
//! `degraded: <reason>\n`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::error::MailboxError;
use crate::store::Store;

/// Health probe outcome.
#[derive(Debug, Clone)]
pub enum HealthStatus {
    /// Everything's fine.
    Ok,
    /// Degraded with a stable, human-readable reason.
    Degraded(String),
}

impl HealthStatus {
    /// Wire reply line ending in `\n`.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            HealthStatus::Ok => "ok\n".to_string(),
            HealthStatus::Degraded(reason) => format!("degraded: {reason}\n"),
        }
    }
}

/// Probe the server's runtime state. The store check is the only
/// dynamic input today; future probes (Arti bootstrap, disk full)
/// extend this function rather than the wire format.
pub fn probe(store: &Store) -> HealthStatus {
    match store.storage_bytes() {
        Ok(_) => HealthStatus::Ok,
        Err(_) => HealthStatus::Degraded("db_unavailable".into()),
    }
}

/// Bind the UDS, set mode 0600, and run the accept loop until
/// `cancel` fires. Returns the join handle.
pub async fn spawn(
    socket_path: PathBuf,
    store: Arc<Store>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, MailboxError> {
    let _ = std::fs::remove_file(&socket_path); // ignore ENOENT
    let listener = UnixListener::bind(&socket_path).map_err(MailboxError::Io)?;
    set_mode_0600(&socket_path)?;

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let store = store.clone();
                            tokio::spawn(handle_one(stream, store));
                        }
                        Err(e) => {
                            tracing::warn!(?e, "health UDS accept error");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
    });
    Ok(handle)
}

async fn handle_one(stream: UnixStream, store: Arc<Store>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    if line.trim() != "GET /health" {
        let _ = write.write_all(b"degraded: unknown_request\n").await;
        return;
    }
    let status = probe(&store);
    let _ = write.write_all(status.line().as_bytes()).await;
}

fn set_mode_0600(path: &Path) -> Result<(), MailboxError> {
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(MailboxError::Io)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::time::{timeout, Duration};

    #[test]
    fn health_status_line_format() {
        assert_eq!(HealthStatus::Ok.line(), "ok\n");
        assert_eq!(
            HealthStatus::Degraded("disk_full".into()).line(),
            "degraded: disk_full\n"
        );
    }

    #[tokio::test]
    async fn probe_reports_ok_for_healthy_store() {
        let store = Store::in_memory().unwrap();
        match probe(&store) {
            HealthStatus::Ok => (),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn end_to_end_uds_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();

        let mut client = UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"GET /health\n").await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let reply = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(reply.starts_with("ok"));

        cancel.cancel();
        timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unknown_request_returns_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();

        let mut client = UnixStream::connect(&sock).await.unwrap();
        client.write_all(b"PING\n").await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let reply = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(reply.starts_with("degraded: unknown_request"));

        cancel.cancel();
        timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn socket_is_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("health.sock");
        let store = Arc::new(Store::in_memory().unwrap());
        let cancel = CancellationToken::new();
        let handle = spawn(sock.clone(), store, cancel.clone()).await.unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        cancel.cancel();
        timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }
}
