// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! Adaptive polling scheduler for our mailboxes.
//!
//! Per-mailbox actor with an Idle (60 s) ↔ Active (15 s) state machine.
//! ±25 % jitter per tick to break timing correlation across mailboxes.
//! Idle ceiling = 5 min when a mailbox is `Unreachable`.

use std::time::Duration;

use rand::Rng;

pub(crate) const ACTIVE_BASE: Duration = Duration::from_secs(15);
pub(crate) const IDLE_BASE: Duration = Duration::from_secs(60);
pub(crate) const IDLE_CEILING: Duration = Duration::from_secs(5 * 60);
pub(crate) const ACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);

/// Compute the next sleep before the per-mailbox actor's next tick.
///
/// Pure function — Task 14 wraps the per-mailbox actor around it.
#[must_use]
pub(crate) fn next_interval(active: bool, unreachable: bool, rng: &mut impl Rng) -> Duration {
    let base = match (active, unreachable) {
        (_, true) => IDLE_CEILING,
        (true, false) => ACTIVE_BASE,
        (false, false) => IDLE_BASE,
    };
    let nanos = base.as_nanos() as i128;
    let jitter_range: i128 = nanos / 4; // ±25 %
    let delta = rng.gen_range(-jitter_range..=jitter_range);
    let out = (nanos + delta).max(0) as u64;
    Duration::from_nanos(out)
}

/// One Challenge → Fetch → Delete cycle. The caller (Task 15's actor)
/// hands each `PendingDeposit` to the inbound MLS dispatcher between
/// Fetch and Delete; this function does NOT decrypt or persist.
///
/// Returned `FetchResponse` is the unmodified server reply — caller
/// inspects it to decide whether to bump Active hold (non-empty
/// deposits) and to drive the inbound dispatch.
pub(crate) async fn run_one_poll_tick<S>(
    client: &mut crate::mailbox::client::MailboxClient<S>,
    signer: &crate::identity::IdentityKey,
) -> crate::error::Result<crate::mailbox::protocol::FetchResponse>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let resp = client.fetch(signer).await?;
    if !resp.deposits.is_empty() {
        let ids: Vec<[u8; 16]> = resp.deposits.iter().map(|d| d.deposit_id).collect();
        client.delete(signer, ids).await?;
    }
    Ok(resp)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn active_interval_within_active_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(true, false, &mut rng);
            assert!(
                d >= Duration::from_millis(11_250) && d <= Duration::from_millis(18_750),
                "active out of band: {d:?}"
            );
        }
    }

    #[test]
    fn idle_interval_within_idle_band() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1000 {
            let d = next_interval(false, false, &mut rng);
            assert!(
                d >= Duration::from_millis(45_000) && d <= Duration::from_millis(75_000),
                "idle out of band: {d:?}"
            );
        }
    }

    #[test]
    fn unreachable_interval_locks_to_idle_ceiling() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(false, true, &mut rng);
            assert!(
                d >= Duration::from_millis(225_000) && d <= Duration::from_millis(375_000)
            );
        }
    }

    #[test]
    fn active_overrides_unreachable_ceiling() {
        // Even with `active=true`, if the actor is `unreachable=true` the ceiling wins.
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..100 {
            let d = next_interval(true, true, &mut rng);
            assert!(d >= Duration::from_millis(225_000));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_tick_drives_full_challenge_fetch_delete_cycle() {
        use crate::identity::IdentityKey;
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{
            ChallengeNonce, DeleteOk, FetchResponse, PendingDeposit,
        };
        use futures::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let cycles_completed = Arc::new(AtomicU32::new(0));
        let counter = cycles_completed.clone();

        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            // 1. Challenge → ChallengeNonce
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            // 2. Fetch → FetchResponse with one deposit
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::FetchResponse(FetchResponse {
                    deposits: vec![PendingDeposit {
                        deposit_id: [1; 16],
                        ciphertext: vec![9],
                        received_at: 1,
                    }],
                }))
                .await
                .unwrap();
            // 3. Delete: Challenge → ChallengeNonce → Delete → DeleteOk
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [1; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::DeleteOk(DeleteOk { deleted: 1, not_found: 0 }))
                .await
                .unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let signer = IdentityKey::generate().unwrap();
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let resp = run_one_poll_tick(&mut client, &signer).await.unwrap();
        assert_eq!(resp.deposits.len(), 1);
        server.await.unwrap();
        assert_eq!(cycles_completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poll_tick_skips_delete_when_no_deposits() {
        use crate::identity::IdentityKey;
        use crate::mailbox::client::MailboxClient;
        use crate::mailbox::codec::{MailboxFrame, MailboxFrameCodec};
        use crate::mailbox::protocol::{ChallengeNonce, FetchResponse};
        use futures::{SinkExt, StreamExt};
        use tokio::io::duplex;
        use tokio_util::codec::Framed;

        let (a, b) = duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(b, MailboxFrameCodec::new());
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::ChallengeNonce(ChallengeNonce {
                    nonce: [0; 32],
                    issued_at: 1,
                }))
                .await
                .unwrap();
            let _ = framed.next().await;
            framed
                .send(MailboxFrame::FetchResponse(FetchResponse { deposits: vec![] }))
                .await
                .unwrap();
            // No more frames expected — if `run_one_poll_tick` calls Delete on an
            // empty deposit list, the test will hang waiting for a request that
            // never arrives. The framed.next() in the test would error out from
            // the EOF path, propagating into `run_one_poll_tick` as an Err.
        });

        let signer = IdentityKey::generate().unwrap();
        let mut client = MailboxClient::from_stream("a.onion".into(), a);
        let resp = run_one_poll_tick(&mut client, &signer).await.unwrap();
        assert!(resp.deposits.is_empty());
        server.await.unwrap();
    }
}
