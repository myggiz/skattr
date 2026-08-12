# #76 — Chunk-Pull Dial-On-Demand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a pending inbound attachment fetch establish its own connection, and stop it reporting `"request timeout"` for requests that were never transmitted.

**Architecture:** Three changes inside the per-peer delivery actor. `ChunkRx` gains a rollback (`unsent`) so a failed transmit costs no retry attempt; `peer.rs` consumes the `bool` that `send_chunk_requests` already returns; the retry tick dials when chunk work is pending, paced by a capped backoff. One new non-consuming `InboundDispatch` probe makes a never-started begin visible without moving it out of durable dispatcher state.

**Tech Stack:** Rust 2021, tokio (paused-time tests), existing `delivery` module. No new dependencies, no wire-format change, no migration.

**Spec:** `docs/superpowers/specs/2026-08-12-issue-76-chunk-pull-dial-design.md`

**Branch:** `76-chunk-pull-dial` — already carries `cdbcc5c` (the red test) and `44781d1` (the spec).

## Global Constraints

- **No `unwrap`/`expect` in library code.** Tests may use them; the modules touched already carry `#![cfg_attr(test, allow(clippy::unwrap_used, …))]`.
- **`cargo clippy -D warnings` is the done-gate**, plus `cargo fmt --all -- --check`, `cargo test`, `cargo deny check`.
- **Never log onions, pubkeys, or payloads at info+.** `ensure_conn` already handles dial-error redaction; do not add new error logging around it.
- **Backoff schedule is exactly `[15_000, 60_000, 300_000, 900_000]` ms**, holding at the last entry — the same shape as `chunk_sweep.rs:22`.
- **`CHUNK_RETRY_BUDGET = 3`, `CHUNK_REQUEST_TIMEOUT = 30s`, `RETRY_TICK = 1s`, `DIAL_TIMEOUT = 30s`** — do not change any of these.
- **All new tests use `#[tokio::test(start_paused = true)]`** so they are deterministic and instant. No wall-clock sleeps.
- **Do not touch the message/outbox dial policy** (`peer.rs:672`). It is deliberately out of scope.
- Every `.rs` file keeps its GPLv3 licence header.

---

### Task 1: `ChunkRx::unsent` — return untransmitted requests to the queue

**Files:**
- Modify: `crates/core/src/delivery/chunk_transfer.rs` (add method after `reissue`, ~line 163; tests in the same file's `mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub(crate) fn unsent(&mut self, indices: &[u32])` on `ChunkRx`.

**Why this shape:** `next_requests` *moves* indices out of the `needed` VecDeque into `inflight` (`chunk_transfer.rs:94-110`). Removing from `inflight` alone would lose them from **both** collections — never re-requested, `is_complete()` never satisfiable, transfer silently deadlocked. The index must go back into `needed`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/core/src/delivery/chunk_transfer.rs`:

```rust
    #[test]
    fn unsent_returns_indices_for_rerequest() {
        // A request window that never reached the wire must be re-requestable,
        // and must not consume any of the retry budget. Multi-chunk on purpose:
        // a single-chunk fixture would pass even if `unsent` reordered the
        // queue, which is exactly the bug worth catching here.
        let payload = vec![7u8; crate::attachment::CHUNK_SIZE * 2 + 5];
        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(&payload, "f", "m").unwrap();
        assert!(manifest.chunks.len() >= 3, "fixture must be multi-chunk");
        let mut rx = ChunkRx::new(manifest, &[]);

        let first = rx.next_requests();
        assert!(!first.is_empty(), "fixture must produce requests");

        rx.unsent(&first);

        let again = rx.next_requests();
        assert_eq!(
            again, first,
            "indices whose send failed must be offered again, in the same order"
        );
    }

    #[test]
    fn unsent_does_not_deadlock_the_transfer() {
        // The failure mode this guards: if `unsent` only dropped the indices
        // from `inflight`, they would be absent from `needed` too — so nothing
        // would ever be requested again and `is_complete` could never be true.
        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(&vec![7u8; 10], "f", "m").unwrap();
        let total = manifest.chunks.len() as u32;
        let mut rx = ChunkRx::new(manifest, &[]);

        let reqs = rx.next_requests();
        rx.unsent(&reqs);

        let reissued = rx.next_requests();
        for idx in &reissued {
            assert!(rx.on_received(*idx), "re-requested index must be in flight");
        }
        assert_eq!(rx.progress(), (total, total));
        assert!(rx.is_complete(), "transfer must still be completable");
    }

    #[test]
    fn unsent_ignores_an_index_that_already_resolved() {
        // A Chunk can arrive between the failed send and the rollback. That
        // index is no longer in flight and must not be pushed back into
        // `needed`, or it would be fetched a second time.
        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(&vec![7u8; 10], "f", "m").unwrap();
        let mut rx = ChunkRx::new(manifest, &[]);

        let reqs = rx.next_requests();
        let first = reqs[0];
        assert!(rx.on_received(first), "chunk arrives before the rollback");

        rx.unsent(&reqs);

        assert!(
            !rx.next_requests().contains(&first),
            "an already-received index must not be re-queued"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p skattr-core --lib chunk_transfer::tests::unsent`
Expected: FAIL — `no method named 'unsent' found for struct 'ChunkRx'`

- [ ] **Step 3: Implement `unsent`**

Add after `reissue` in `impl ChunkRx`:

```rust
    /// Roll back bookkeeping for requests that were never transmitted.
    ///
    /// Returns each index to `needed` so it is requested again, and charges no
    /// attempt: a send that failed locally is not evidence about the peer, so
    /// it must not consume the retry budget that exists to detect peer silence.
    ///
    /// Returning them to `needed` is load-bearing, not tidiness —
    /// `next_requests` *moves* indices out of `needed` into `inflight`, so
    /// dropping them from `inflight` alone would leave them in neither
    /// collection and the transfer could never complete.
    pub(crate) fn unsent(&mut self, indices: &[u32]) {
        // Reverse + `push_front` preserves the original relative order: pushing
        // [0,1,2] onto the front in forward order would leave [2,1,0].
        for &index in indices.iter().rev() {
            // `is_some()`: an index that already resolved (a Chunk arrived
            // between the failed send and this rollback) is no longer in
            // flight and must not be re-queued.
            if self.inflight.remove(&index).is_some() {
                self.needed.push_front(index);
            }
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p skattr-core --lib chunk_transfer`
Expected: PASS, including the pre-existing `chunk_transfer` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/chunk_transfer.rs
git commit -s -m "feat(delivery): add ChunkRx::unsent to roll back untransmitted requests"
```

---

### Task 2: Consume the send result so a failed transmit costs no attempt

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` — the five `send_chunk_requests` call sites (`147`, `727`, `814`, `1010`, `1020`) and one new test in `mod tests`

**Interfaces:**
- Consumes: `ChunkRx::unsent` (Task 1).
- Produces: no new API. After this task, `"request timeout"` is only reachable when a request actually went out.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/core/src/delivery/peer.rs`:

```rust
    /// #76: `"request timeout"` must mean the peer stayed silent — never that
    /// we failed to transmit. A fetch whose connection dies must not burn its
    /// 3 x 30s budget on requests that never left the machine and then report
    /// a timeout the sender has no record of.
    #[tokio::test(start_paused = true)]
    async fn a_dead_connection_does_not_produce_a_false_request_timeout() {
        use crate::attachment::store::ChunkStore;
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;
        use tokio::io::DuplexStream;

        let payload = vec![3u8; crate::attachment::CHUNK_SIZE + 10];
        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(&payload, "f.bin", "text/plain").unwrap();
        let aid = manifest.attachment_id;

        let pool = Arc::new(Pool::in_memory());
        crate::storage::attachments::AttachmentRepo::new(&pool)
            .insert(
                &aid,
                "in",
                &manifest.to_cbor().unwrap(),
                manifest.chunks.len() as i64,
                0,
            )
            .unwrap();

        struct Stub {
            begin: StdMutex<Option<crate::delivery::chunk_transfer::AttachmentBegin>>,
            failed: Arc<StdMutex<Option<String>>>,
        }
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                Some(MessageId::generate())
            }
            #[allow(private_interfaces)]
            fn take_begin_attachment(
                &self,
                _peer: PublicKey,
            ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
                self.begin.lock().unwrap().take()
            }
            fn attachment_failed(&self, _aid: [u8; 16], reason: &str) {
                *self.failed.lock().unwrap() = Some(reason.to_string());
            }
        }

        let failed: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let stub: std::sync::Arc<dyn InboundDispatch> = Arc::new(Stub {
            begin: StdMutex::new(Some(crate::delivery::chunk_transfer::AttachmentBegin {
                attachment_id: aid,
                manifest: manifest.clone(),
            })),
            failed: failed.clone(),
        });

        let actor_id = IdentityKey::generate().unwrap();
        let responder_id = IdentityKey::generate().unwrap();
        let responder_static = responder_id.noise_static_public();
        let peer = PublicKey(responder_id.public().0);
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let responder_task = tokio::spawn(async move {
            handshake_responder(server_stream, &responder_id, None)
                .await
                .unwrap()
        });
        let (conn, _) = handshake_initiator(client_stream, &actor_id, &responder_static, None)
            .await
            .unwrap();
        let (mut peer_conn, _) = responder_task.await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let (_jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(4);
        let (_welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<DuplexStream>>(4);
        let run_pool = pool.clone();
        let run_dir = tmp.path().join("downloads");
        let run_store = Arc::new(ChunkStore::new(tmp.path()));
        let _actor = tokio::spawn(async move {
            // No dialer: the fetch cannot recover, which is precisely the
            // condition under which it must NOT invent a timeout.
            let _ = super::full_run::<DuplexStream>(
                peer,
                Some(conn),
                jobs_rx,
                welcome_rx,
                ctrl_rx,
                run_pool,
                Some(stub),
                None,
                Duration::ZERO,
                None,
                Some(run_store),
                Some(run_dir),
            )
            .await;
        });

        // Start the fetch over the live conn, then kill the connection.
        peer_conn
            .send(Frame::MlsApp(b"manifest".to_vec()))
            .await
            .unwrap();
        // Drain until the first ChunkRequest proves the fetch began.
        loop {
            let f = tokio::time::timeout(Duration::from_secs(5), peer_conn.recv())
                .await
                .expect("actor must respond")
                .unwrap()
                .expect("conn must not EOF yet");
            if matches!(f, Frame::ChunkRequest { .. }) {
                break;
            }
        }
        drop(peer_conn); // actor sees EOF -> conn = None

        // Well past 3 x CHUNK_REQUEST_TIMEOUT.
        tokio::time::sleep(Duration::from_secs(150)).await;

        assert!(
            failed.lock().unwrap().is_none(),
            "#76: a fetch that could not transmit must not report a timeout \
             the peer never saw (got {:?})",
            failed.lock().unwrap()
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p skattr-core --lib a_dead_connection_does_not_produce_a_false_request_timeout`
Expected: FAIL — the assertion trips with `Some("request timeout")`, because the budget is spent against a `None` connection.

- [ ] **Step 3: Consume the result at all five call sites**

In `maybe_start_next_rx` (~line 147), replace `let _ = send_chunk_requests(conn, aid, &reqs).await;` with:

```rust
        if !send_chunk_requests(conn, aid, &reqs).await {
            rx.unsent(&reqs);
        }
```

In the retry tick's timeout arm (~line 727):

```rust
                            crate::delivery::chunk_transfer::ChunkAction::Request(idxs)
                                if !idxs.is_empty() =>
                            {
                                if !send_chunk_requests(&mut conn, aid, &idxs).await {
                                    if let Some(rx) = active_rx.as_mut() {
                                        rx.unsent(&idxs);
                                    }
                                }
                            }
```

In the `ReplaceConn` arm (~line 811), switch the borrow to `as_mut` so the rollback is possible:

```rust
                        if let Some(rx) = active_rx.as_mut() {
                            let aid = rx.attachment_id();
                            let reqs = rx.reissue();
                            if !send_chunk_requests(&mut conn, aid, &reqs).await {
                                rx.unsent(&reqs);
                            }
                        }
```

In the window-refill arm (~line 1007):

```rust
                                        let reqs = rx.next_requests();
                                        let aid = rx.attachment_id();
                                        if !send_chunk_requests(&mut conn, aid, &reqs).await {
                                            rx.unsent(&reqs);
                                        }
                                        active_rx = Some(rx);
```

In the `on_bad` retry arm (~line 1019):

```rust
                                            let aid = rx.attachment_id();
                                            if !send_chunk_requests(&mut conn, aid, &idxs).await {
                                                rx.unsent(&idxs);
                                            }
                                            active_rx = Some(rx);
```

Note the `rx` bindings in the last two arms are already owned locals; add `mut` if the compiler asks.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p skattr-core --lib delivery::peer`
Expected: PASS for the new test and every pre-existing `delivery::peer` test. `inbound_fetch_dials_when_there_is_no_connection` still FAILS — Task 4 fixes it.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -s -m "fix(delivery): don't charge a retry attempt for an untransmitted chunk request"
```

---

### Task 3: `InboundDispatch::has_pending_begin` — see a queued begin without consuming it

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (trait, near `take_begin_attachment` ~line 327)
- Modify: `crates/core/src/daemon/inbound.rs` (impl, near `take_begin_attachment` ~line 844)

**Interfaces:**
- Produces: `fn has_pending_begin(&self, _peer: PublicKey) -> bool { false }` on `InboundDispatch`; real implementation on `DaemonInbound`. Task 4 consumes it.
- The default `false` keeps every other implementor compiling untouched (`mailbox/poll.rs:162`, `daemon/accept.rs:178`, the test stubs, and `crates/tests/`).

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/core/src/daemon/inbound.rs`:

```rust
    #[test]
    fn has_pending_begin_reports_without_consuming() {
        use crate::delivery::peer::InboundDispatch;
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let (events_tx, _rx) = tokio::sync::broadcast::channel(4);
        let inbound = DaemonInbound::new(pool, events_tx);
        let peer = PublicKey([9u8; 32]);

        assert!(!inbound.has_pending_begin(peer), "empty queue reports false");

        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(b"x", "f", "m").unwrap();
        inbound.requeue_attachment(
            peer,
            crate::delivery::chunk_transfer::AttachmentBegin {
                attachment_id: manifest.attachment_id,
                manifest,
            },
        );

        assert!(inbound.has_pending_begin(peer), "queued begin must be visible");
        assert!(
            inbound.has_pending_begin(peer),
            "the probe must not consume — a second call still reports true"
        );
        assert!(
            inbound.take_begin_attachment(peer).is_some(),
            "the begin must still be there to drain"
        );
        assert!(!inbound.has_pending_begin(peer), "drained queue reports false");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p skattr-core --lib has_pending_begin_reports_without_consuming`
Expected: FAIL — `no method named 'has_pending_begin'`

- [ ] **Step 3: Add the trait method and the implementation**

In `crates/core/src/delivery/peer.rs`, in `trait InboundDispatch`, after `take_begin_attachment`:

```rust
    /// Whether `peer` has an inbound attachment waiting to start, *without*
    /// consuming it.
    ///
    /// The actor needs to know a fetch is pending in order to dial for it
    /// (#76), but must not drain the begin to find out: draining moves it from
    /// the dispatcher's durable queue into actor-local memory, where an actor
    /// crash would lose it.
    fn has_pending_begin(&self, _peer: PublicKey) -> bool {
        false
    }
```

In `crates/core/src/daemon/inbound.rs`, in `impl InboundDispatch for DaemonInbound`:

```rust
    fn has_pending_begin(&self, peer: PublicKey) -> bool {
        // A poisoned lock reports "nothing pending" rather than panicking:
        // this only gates an optimistic dial, and `take_begin_attachment`
        // already degrades the same way.
        self.begins
            .lock()
            .ok()
            .and_then(|g| g.get(&peer).map(|q| !q.is_empty()))
            .unwrap_or(false)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p skattr-core --lib inbound`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/daemon/inbound.rs
git commit -s -m "feat(delivery): add non-consuming InboundDispatch::has_pending_begin"
```

---

### Task 4: Dial on demand for pending chunk work, with capped backoff

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` — constant near the other actor constants (~line 524), two locals in `full_run` (~line 550), the dial block inside the retry tick's `chunk_enabled` section (~line 716), one new test

**Interfaces:**
- Consumes: `has_pending_begin` (Task 3), `ensure_conn` (`peer.rs:1138`).
- Produces: no new API. Turns `inbound_fetch_dials_when_there_is_no_connection` (already on the branch, `cdbcc5c`) green.

- [ ] **Step 1: Write the failing backoff test**

Add to `mod tests` in `crates/core/src/delivery/peer.rs`:

```rust
    /// #76: dial attempts for a pending fetch must be paced. A failed Tor dial
    /// blocks the actor inline for up to DIAL_TIMEOUT (30s), so dialing once
    /// per RETRY_TICK (1s) against an offline peer would be a dial storm.
    #[tokio::test(start_paused = true)]
    async fn pending_fetch_dials_are_paced_by_backoff() {
        use crate::attachment::store::ChunkStore;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;
        use tokio::io::DuplexStream;

        let (manifest, _cts) =
            crate::attachment::chunker::chunk_plaintext(b"pace me", "f.bin", "text/plain").unwrap();
        let aid = manifest.attachment_id;

        let pool = Arc::new(Pool::in_memory());
        crate::storage::attachments::AttachmentRepo::new(&pool)
            .insert(&aid, "in", &manifest.to_cbor().unwrap(), 1, 0)
            .unwrap();

        struct Stub {
            begin: StdMutex<Option<crate::delivery::chunk_transfer::AttachmentBegin>>,
        }
        impl InboundDispatch for Stub {
            fn dispatch(&self, _peer: PublicKey, _ct: &[u8]) -> Option<MessageId> {
                None
            }
            #[allow(private_interfaces)]
            fn take_begin_attachment(
                &self,
                _peer: PublicKey,
            ) -> Option<crate::delivery::chunk_transfer::AttachmentBegin> {
                self.begin.lock().unwrap().take()
            }
            fn has_pending_begin(&self, _peer: PublicKey) -> bool {
                self.begin.lock().map(|g| g.is_some()).unwrap_or(false)
            }
        }

        struct FailingDial {
            calls: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl crate::delivery::dial::OutboundDial<DuplexStream> for FailingDial {
            async fn dial(
                &self,
                _peer: PublicKey,
            ) -> Result<(
                AuthenticatedConnection<DuplexStream>,
                zeroize::Zeroizing<[u8; 32]>,
            )> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(crate::delivery::DeliveryErrorKind::Timeout.into())
            }
            async fn dial_at(
                &self,
                peer: PublicKey,
                _onion: &str,
            ) -> Result<(
                AuthenticatedConnection<DuplexStream>,
                zeroize::Zeroizing<[u8; 32]>,
            )> {
                self.dial(peer).await
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let dialer = Arc::new(FailingDial {
            calls: calls.clone(),
        });
        let peer = PublicKey([4u8; 32]);
        let tmp = tempfile::tempdir().unwrap();
        let (_jobs_tx, jobs_rx) = mpsc::channel::<DeliveryJob>(4);
        let (_welcome_tx, welcome_rx) = mpsc::channel::<WelcomeJob>(4);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<PeerCtrl<DuplexStream>>(4);
        let stub: std::sync::Arc<dyn InboundDispatch> = Arc::new(Stub {
            begin: StdMutex::new(Some(crate::delivery::chunk_transfer::AttachmentBegin {
                attachment_id: aid,
                manifest: manifest.clone(),
            })),
        });
        let run_pool = pool.clone();
        let run_dir = tmp.path().join("downloads");
        let run_store = Arc::new(ChunkStore::new(tmp.path()));
        let _actor = tokio::spawn(async move {
            let _ = super::full_run::<DuplexStream>(
                peer,
                None,
                jobs_rx,
                welcome_rx,
                ctrl_rx,
                run_pool,
                Some(stub),
                Some(dialer),
                Duration::ZERO,
                None,
                Some(run_store),
                Some(run_dir),
            )
            .await;
        });

        tokio::time::sleep(Duration::from_secs(1200)).await;

        // 1200s of ticks. Un-paced that is ~1200 dials; the backoff schedule
        // (15s, 60s, 300s, 900s, then hold) allows roughly 5.
        let n = calls.load(Ordering::SeqCst);
        assert!(n >= 2, "must keep retrying a pending fetch (got {n})");
        assert!(
            n < 15,
            "dials must be paced by backoff, not issued per retry tick (got {n})"
        );
    }
```

- [ ] **Step 2: Run both dial tests to verify they fail**

Run: `cargo test -p skattr-core --lib -- inbound_fetch_dials_when_there_is_no_connection pending_fetch_dials_are_paced_by_backoff`
Expected: BOTH FAIL — zero dial calls, because nothing on the chunk path dials.

- [ ] **Step 3: Add the backoff constant**

Next to the other actor constants in `crates/core/src/delivery/peer.rs` (~line 524):

```rust
/// Pacing for dials issued on behalf of a pending inbound attachment (#76).
/// Same shape as `chunk_sweep`'s deposit backoff, held at the last entry. A
/// failed Tor dial already costs up to `DIAL_TIMEOUT` (30s) inline, so an
/// un-paced dial on every `RETRY_TICK` would be a dial storm against a peer
/// that is simply offline.
const CHUNK_DIAL_BACKOFF_MS: &[u64] = &[15_000, 60_000, 300_000, 900_000];
```

- [ ] **Step 4: Add the actor-local backoff state**

Beside the other `full_run` locals (~line 550, near `let mut pending`):

```rust
    // #76 dial pacing. Actor-local, not persisted: a restarted actor gets a
    // fresh schedule, which is correct — a restart usually means conditions
    // changed.
    let mut next_chunk_dial_at: Option<tokio::time::Instant> = None;
    let mut chunk_dial_step: usize = 0;
```

- [ ] **Step 5: Dial when chunk work is pending**

Inside the retry tick, as the **first** thing in the `if chunk_enabled {` block (~line 716), before the timeout handling — so a successful dial lets this same tick send requests:

```rust
                    // #76: a pending fetch is background work with no local
                    // user action behind it, so nothing else will ever give it
                    // a connection. Dial for it, paced by backoff.
                    let work_pending = active_rx.is_some()
                        || inbound
                            .as_ref()
                            .map(|d| d.has_pending_begin(peer))
                            .unwrap_or(false);
                    if work_pending && conn.is_none() {
                        let due = next_chunk_dial_at
                            .map(|t| tokio::time::Instant::now() >= t)
                            .unwrap_or(true);
                        if due {
                            if ensure_conn::<S>(peer, &mut conn, &dialer).await {
                                next_chunk_dial_at = None;
                                chunk_dial_step = 0;
                            } else {
                                let idx = chunk_dial_step.min(CHUNK_DIAL_BACKOFF_MS.len() - 1);
                                // Deadline measured from now: the dial itself
                                // may have just burned up to DIAL_TIMEOUT.
                                next_chunk_dial_at = Some(
                                    tokio::time::Instant::now()
                                        + std::time::Duration::from_millis(
                                            CHUNK_DIAL_BACKOFF_MS[idx],
                                        ),
                                );
                                chunk_dial_step =
                                    (chunk_dial_step + 1).min(CHUNK_DIAL_BACKOFF_MS.len() - 1);
                            }
                        }
                    }
```

The existing `if conn.is_some()` drain further down (~line 754) then picks up the begin and starts the fetch on the same tick. Leave that gate as it is.

- [ ] **Step 6: Re-issue a rolled-back window**

Immediately after the dial block from Step 5, still inside `if chunk_enabled`:

```rust
                    // A window rolled back by `unsent` (Task 2) left `inflight`
                    // empty, and nothing else re-issues it: `timed_out` iterates
                    // `inflight`, `reissue` returns `inflight` keys, and the
                    // `maybe_start_next_rx` drain below only runs when
                    // `active_rx` is None. Without this, a fetch that failed to
                    // transmit would hold a live connection and never ask again.
                    if conn.is_some() {
                        if let Some(rx) = active_rx.as_mut() {
                            let reqs = rx.next_requests();
                            if !reqs.is_empty() {
                                let aid = rx.attachment_id();
                                if !send_chunk_requests(&mut conn, aid, &reqs).await {
                                    rx.unsent(&reqs);
                                }
                            }
                        }
                    }
```

`next_requests` returns empty once the in-flight window is full (`CHUNK_WINDOW`), so this is a no-op on every tick of a healthy transfer.

- [ ] **Step 7: Add the recovery test**

This is the end-to-end proof that the three parts compose — the previous tests each cover one part in isolation. Add to `mod tests` in `crates/core/src/delivery/peer.rs`:

```rust
    /// #76 end-to-end: a fetch whose connection dies must re-dial and finish.
    /// This is the exact field scenario — manifest arrives, connection drops
    /// mid-pull, and the transfer has to recover on its own.
    #[tokio::test(start_paused = true)]
    async fn a_fetch_recovers_after_its_connection_dies() {
        // Build with the same harness shape as
        // `resume_reissues_chunk_requests_and_completes_after_replace_conn`:
        // a real chunked attachment, a Stub InboundDispatch yielding one begin
        // and recording `attachment_received`, and a dialer whose FIRST dial
        // fails (peer still unreachable) and whose SECOND hands back a live
        // pre-built connection.
        //
        // Sequence:
        //   1. spawn the actor with conn1; send the manifest MlsApp;
        //      collect the first ChunkRequest window over conn1
        //   2. drop the conn1 peer side -> the actor sees EOF, conn = None
        //   3. advance time past the first backoff step (15s) twice, so the
        //      failing dial is followed by a succeeding one
        //   4. serve every requested chunk over conn2
        //   5. assert `attachment_received` fired with the right id, and that
        //      `attachment_failed` never did
        //
        // Assert on both: completing while also emitting a failure would still
        // be wrong.
    }
```

Write the body following the cited existing test's structure (`dial` helper, `collect_requests` helper, chunk-serving loop). Keep the two assertions stated above.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p skattr-core --lib delivery::peer`
Expected: PASS — including `inbound_fetch_dials_when_there_is_no_connection` (the original red test), `pending_fetch_dials_are_paced_by_backoff`, `a_fetch_recovers_after_its_connection_dies`, `a_dead_connection_does_not_produce_a_false_request_timeout`, and every pre-existing test (`resume_reissues_chunk_requests_and_completes_after_replace_conn`, `retry_requeued_begin_starts_fetching_on_the_next_tick`, the CAS/completion tests).

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/delivery/peer.rs
git commit -s -m "fix(delivery): dial on demand for a pending attachment fetch (#76)"
```

---

### Task 5: Full gate and CHANGELOG

**Files:**
- Modify: `CHANGELOG.md` (the `## [Unreleased] — targeting v0.1.15` section, `### Fixed`)

**Interfaces:** none.

- [ ] **Step 1: Add the CHANGELOG entry**

Under `### Fixed`, in the user-facing voice the surrounding entries use (describe the symptom, not the internals):

```markdown
- **Received files could get stuck, or wrongly report a timeout** (#76): if the
  connection to the sender dropped while a file was arriving — common over Tor —
  the download had no way to reconnect. It would sit unfinished, or give up with
  a "request timeout" that the sender had no record of, because the requests
  never actually left your machine. Downloads now reconnect on their own, with a
  back-off so an offline peer isn't hammered, and a transfer only reports a
  timeout when the sender genuinely didn't answer.
```

- [ ] **Step 2: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo test
cargo deny check
```

Expected: fmt clean; clippy 0 warnings; all suites 0 failed; deny ok. Run these in the **foreground** and paste the real output — no success claims without it.

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -s -m "docs(changelog): record the attachment reconnect fix (#76)"
```

---

## Verification checklist

Maps to the spec's acceptance table (§8):

- [ ] A pending inbound attachment with no connection causes a dial — `inbound_fetch_dials_when_there_is_no_connection`
- [ ] A fetch whose connection dies recovers instead of timing out — `a_dead_connection_does_not_produce_a_false_request_timeout` + Task 4's dial
- [ ] `"request timeout"` only when a request was transmitted — Task 2
- [ ] Dials are paced, not per-tick — `pending_fetch_dials_are_paced_by_backoff`
- [ ] A never-started begin is detected without consuming it — `has_pending_begin_reports_without_consuming`
- [ ] Existing behaviour over a live connection is unchanged — the pre-existing `delivery::peer` chunk tests
- [ ] `unsent` cannot deadlock a transfer — `unsent_does_not_deadlock_the_transfer`
- [ ] A rolled-back window is re-issued rather than stalling — `a_fetch_recovers_after_its_connection_dies` (Task 4 Step 6)
