// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! A scope guard that runs a cleanup closure on drop.
//!
//! Exists so that cleanup of **decrypted plaintext** happens on every error
//! exit path — `?` and early return — rather than only where someone
//! remembered to write it. Attachments are kept encrypted at rest, so a
//! failure part-way through producing plaintext must not leave that plaintext
//! behind (#156, #52). It also covers a panic, but only in builds that
//! unwind: this workspace's release profile sets `panic = "abort"`
//! (`Cargo.toml`), and `Drop` does not run on abort — so in a shipped release
//! binary a panic still kills the process without running this cleanup. The
//! error-path coverage, which is the reported defect, is unaffected by that.

/// Runs `f` when dropped, unless [`OnDrop::disarm`] was called first.
///
/// The closure **must not panic**: a panic inside `Drop` during unwinding
/// aborts the process. Cleanup here is `let _ = std::fs::remove*`, which
/// cannot panic.
pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    /// Arm a guard that will run `f` on drop.
    pub(crate) fn new(f: F) -> Self {
        Self(Some(f))
    }

    /// Cancel the cleanup.
    ///
    /// Takes `self` by value so a disarmed guard cannot be reused, and so the
    /// call site reads as the moment responsibility for the plaintext is
    /// handed over (e.g. immediately after a successful `rename`).
    pub(crate) fn disarm(mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn runs_the_closure_on_drop() {
        let hits = Arc::new(AtomicUsize::new(0));
        {
            let h = hits.clone();
            let _g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disarm_cancels_the_closure() {
        let hits = Arc::new(AtomicUsize::new(0));
        {
            let h = hits.clone();
            let g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
            g.disarm();
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "disarmed guard must not run"
        );
    }

    #[test]
    fn runs_during_unwind() {
        // The property that distinguishes this from explicit cleanup at each
        // error site: a panic between arming and disarming still cleans up
        // *while unwinding*. This test runs under the dev/test profile, which
        // unwinds; the release profile sets `panic = "abort"` (Cargo.toml),
        // where `Drop` does not run, so this guarantee does not extend to a
        // shipped release binary — only to the error-path (`?`) case.
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = OnDrop::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
            panic!("boom");
        }));
        assert!(result.is_err(), "the panic must propagate");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "cleanup must run while unwinding"
        );
    }
}
