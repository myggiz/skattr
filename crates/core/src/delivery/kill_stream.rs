// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Test-only: an `AsyncRead + AsyncWrite` wrapper that can be "killed"
//! mid-stream by flipping a shared atomic. After the kill flag is set,
//! all reads and writes return `ErrorKind::BrokenPipe`.
//!
//! Gated on `feature = "test-harness"` because it only exists to
//! exercise the kill-mid-message integration test.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Handle that kills both ends of a paired `KillableStream`.
#[derive(Clone, Debug, Default)]
pub struct KillSwitch(Arc<AtomicBool>);

impl KillSwitch {
    /// Create a new, un-killed `KillSwitch`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the kill flag. Idempotent.
    pub fn kill(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if the kill flag has been set.
    #[must_use]
    pub fn is_killed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Wrap `inner` so that once `switch` is flipped, all I/O fails.
pub struct KillableStream<S> {
    inner: S,
    switch: KillSwitch,
}

impl<S> KillableStream<S> {
    /// Wrap `inner` with the given `switch`. All I/O fails after `switch` is killed.
    pub fn new(inner: S, switch: KillSwitch) -> Self {
        Self { inner, switch }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for KillableStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for KillableStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        if this.switch.is_killed() {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
