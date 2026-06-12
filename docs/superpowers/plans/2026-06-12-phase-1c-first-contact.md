# Phase 1C — First Contact Over Direct Transport — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two real daemons complete `invite → add → Welcome → bidirectional message` over direct transport, proven by a non-`#[ignore]` CI guardrail driving the real `run_with_transport` assembly.

**Architecture:** Embed the inviter's signed `ContactCard` in the invite link (ADR 0008) so the invitee's dialer can resolve the inviter's onion; persist it on `AddContact`; send the invitee's own self-card back to the new contact; and make the Welcome-send arm dial on demand. These close the three first-contact gaps the Phase 1B guardrail exposed.

**Tech Stack:** Rust, tokio, OpenMLS, `snow`, `ciborium`, rusqlite. The new guardrail uses the in-process `LoopbackTransport` + `run_loopback` (no Tor).

**Specs:** `docs/superpowers/specs/2026-06-12-phase-1c-first-contact-design.md`; `docs/adr/0008-invite-embeds-contact-card.md`. Read both.

**Conventions (read once):**
- cargo is NOT on PATH — prefix every cargo command with `. "$HOME/.cargo/env" && `.
- Run tests/clippy with `--features test-harness`.
- NO `unwrap()`/`expect()` in non-test (`src/`) code — `?` + typed `CoreError`. Test code may `.unwrap()` with a `#[allow(clippy::unwrap_used, clippy::expect_used)]` on the tests module.
- Never log onions/pubkeys/ciphertext at info+; `warn!` carries static text / onion-free error strings only.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

## File map

- **Modify** `crates/core/src/invite/link.rs` — `InviteLinkBody` embeds `card`; `generate`/`to_url`/`from_url` rework; unit tests (Task 1).
- **Modify** `crates/core/src/daemon/dispatch.rs` — `create_invite` builds + embeds the self-card; `add_contact` reads `link.body.card.body.*`, persists the inviter card, and sends the invitee's self-card; extract two card helpers (Tasks 1–3).
- **Modify** `crates/core/src/delivery/peer.rs` — Welcome-arm dial-on-demand (Task 4).
- **Create** `crates/tests/src/first_contact_direct.rs` — the live first-contact guardrail (Task 5).
- **Modify** `crates/tests/src/lib.rs` — register the new test module (Task 5).

---

## Task 1: Invite link embeds the inviter's signed ContactCard (ADR 0008)

**Why:** The dialer resolves a peer's onion only from a signed `ContactCard` (`latest_card`). The invite must therefore carry the inviter's signed card, not a bare onion. This is a wire-format change to `InviteLinkBody`; it must land end-to-end (struct + generate + to_url + from_url + the `create_invite` builder + the `add_contact` readers) so the crate compiles.

**Files:**
- Modify: `crates/core/src/invite/link.rs`
- Modify: `crates/core/src/daemon/dispatch.rs` (`create_invite` ~line 201; `add_contact` ~line 269)
- Test: `crates/core/src/invite/link.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Change `InviteLinkBody` to embed the card**

In `crates/core/src/invite/link.rs`, replace the `identity` + `onion` fields with `card`:

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct InviteLinkBody {
    /// Inviter's signed self-card (carries identity + onion + mailboxes +
    /// version). Supersedes the bare identity+onion (ADR 0008).
    pub card: crate::contact::ContactCard,
    /// Single-use MLS KeyPackage (binary, TLS-codec bytes).
    #[serde(with = "serde_bytes")]
    pub key_package: Vec<u8>,
    /// 32-byte one-time secret mixed into Noise PSK + first MLS Commit.
    pub psk: [u8; 32],
    /// Unix timestamp (seconds) after which the invite is invalid.
    pub expires_at: i64,
}
```

- [ ] **Step 2: Rework `InviteLink::generate` to take the card**

Replace the `generate` signature + body:

```rust
pub fn generate(
    inviter: &IdentityKey,
    card: crate::contact::ContactCard,
    key_package: Vec<u8>,
    psk: [u8; 32],
    ttl_secs: u64,
    now: i64,
) -> Result<Self> {
    let expires_at = now
        .checked_add(i64::try_from(ttl_secs).map_err(|_| {
            CoreError::Invite(InviteErrorKind::Other("ttl overflows i64".into()))
        })?)
        .ok_or_else(|| {
            CoreError::Invite(InviteErrorKind::Other("expires_at overflows i64".into()))
        })?;

    let body = InviteLinkBody {
        card,
        key_package,
        psk,
        expires_at,
    };
    let signature = inviter
        .sign_cbor(&body)
        .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("sign: {e}"))))?;
    Ok(Self {
        body,
        signature,
        psk: InvitePsk(psk),
    })
}
```

> **VERIFY:** `inviter.public()` must equal `card.body.identity` (the inviter signs an invite carrying its own card). Add a guard at the top of `generate`:
> ```rust
> if card.body.identity != inviter.public() {
>     return Err(CoreError::Invite(InviteErrorKind::Other("card identity != inviter".into())));
> }
> ```

- [ ] **Step 3: Rework `to_url` — carry the card as one CBOR param**

Replace the `id=` + `onion=` params with a single `card=` (b64url of the card's canonical CBOR):

```rust
pub fn to_url(&self) -> Result<String> {
    let mut card_blob = Vec::new();
    ciborium::ser::into_writer(&self.body.card, &mut card_blob)
        .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("card cbor: {e}"))))?;
    let card = encode_b64url(&card_blob);
    let kp = encode_b64url(&self.body.key_package);
    let psk = encode_b64url(&self.psk.0);
    let sig = encode_b64url(&self.signature.0);
    Ok(format!(
        "{prefix}card={card}&kp={kp}&psk={psk}&exp={exp}&sig={sig}",
        prefix = URL_PREFIX,
        card = card,
        kp = kp,
        psk = psk,
        exp = self.body.expires_at,
        sig = sig,
    ))
}
```

- [ ] **Step 4: Rework `from_url` — decode the card param, verify the link signature**

In `from_url`, replace the `id`/`onion` parsing with `card` parsing. Keep the `kp`/`psk`/`exp`/`sig` parsing unchanged. The reconstructed `body` uses the decoded card; the link signature is verified against `body.card.body.identity`:

```rust
    // ... inside the key=value loop, replace the `"id"` and `"onion"` arms with:
    //   "card" => card_str = Some(value),
    // (remove id_str / onion handling)

    let card_str = card_str
        .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("missing field card".into())))?;
    let card_blob = decode_b64url(card_str)
        .ok_or_else(|| CoreError::Invite(InviteErrorKind::Other("malformed card".into())))?;
    let card: crate::contact::ContactCard = ciborium::de::from_reader(&card_blob[..])
        .map_err(|e| CoreError::Invite(InviteErrorKind::Other(format!("card decode: {e}"))))?;

    // kp / psk / exp / sig parsed exactly as before ...

    let body = InviteLinkBody {
        card,
        key_package,
        psk,
        expires_at,
    };

    // Verify the inviter's signature over the whole body, keyed by the card's identity.
    IdentityKey::verify_cbor(&body.card.body.identity, &body, &signature)
        .map_err(|_| CoreError::Invite(InviteErrorKind::SignatureInvalid))?;

    // Expiry check (invite expiry, distinct from card expiry).
    if now > body.expires_at {
        return Err(CoreError::Invite(InviteErrorKind::Expired));
    }

    let guard = InvitePsk(body.psk);
    let mut body = body;
    body.psk.zeroize();
    Ok(Self { body, signature, psk: guard })
```

> **VERIFY:** declare a `let mut card_str: Option<&str> = None;` alongside the other field vars; remove `id_str`/`onion` vars and their `.ok_or_else` unwraps. `encode_b64url`/`decode_b64url` already exist in this file. `kp_hash()` (used by `record_received`/`mark_consumed`) hashes `self.body.key_package` — unaffected by the card change; confirm it doesn't reference `body.identity`/`body.onion`.

- [ ] **Step 5: Update `create_invite` to build + embed the self-card**

In `crates/core/src/daemon/dispatch.rs::create_invite`, after computing `onion` and before `InviteLink::generate`, build the inviter's self-card (mirroring `publish_self_card_update`'s onion+mailbox idiom), and pass it to `generate`:

```rust
    // Gather reachable mailboxes (same idiom as publish_self_card_update).
    let mailboxes: Vec<String> = crate::storage::MailboxRepo::new(&handle.pool)
        .list_mine()
        .map_err(map_err)?
        .into_iter()
        .filter(|r| r.status == crate::storage::MailboxStatus::Reachable)
        .map(|r| r.onion)
        .collect();

    // Build the inviter's signed self-card carrying the current onion.
    let card = crate::contact::self_card::build_next_self_card(
        &handle.pool,
        &handle.identity,
        onion,
        mailboxes,
        crate::contact::self_card::DEFAULT_TTL_SECS,
        now,
    )
    .map_err(map_err)?;

    let link = InviteLink::generate(&handle.identity, card, kp_bytes.clone(), psk_raw, ttl, now)
        .map_err(map_err)?;
```

> **VERIFY:** `onion` is currently a `String` consumed by the old `generate` call — now it's moved into `build_next_self_card`. `now` is the `i64` already in scope (`now_unix_seconds()`). `DEFAULT_TTL_SECS` is `pub(crate)` in `contact::self_card` (used by `publish_self_card_update`). The rest of `create_invite` (kp generation, `kp_ref`, `OutstandingInviteRepo::put_with_provider`, the `InviteCreated` result) is unchanged.

- [ ] **Step 6: Update `add_contact`'s six `link.body.{identity,onion}` reads**

In `add_contact`, change the six references (all in this fn) from `link.body.identity` → `link.body.card.body.identity` and `link.body.onion` → `link.body.card.body.onion`:
- the `Contact { identity: link.body.card.body.identity, … }`
- `contact_repo.set_group_id(&link.body.card.body.identity, &group_id)`
- `Event::ContactUpdated(link.body.card.body.identity)`
- `handle.hub.send_welcome(link.body.card.body.identity, welcome)`
- `ContactSummary { pubkey: link.body.card.body.identity, onion: link.body.card.body.onion.clone(), … }`

(Persisting the card + the self-card send come in Tasks 2–3 — this step is just the field-path fixes so it compiles.)

- [ ] **Step 7: Update / add the invite round-trip unit test**

Find the existing `#[cfg(test)] mod tests` in `link.rs`. Any test constructing `InviteLinkBody { identity, onion, … }` or calling `generate(.., onion, ..)` must switch to building a card. Add/replace a round-trip test:

```rust
    #[test]
    fn invite_round_trips_embedded_card() {
        let inviter = IdentityKey::generate().unwrap();
        let card = crate::contact::ContactCard::sign(
            &inviter,
            "inviter.onion".into(),
            vec![],
            1,
            86_400,
            1_000,
        )
        .unwrap();
        let link = InviteLink::generate(&inviter, card, vec![9u8; 4], [7u8; 32], 600, 1_000).unwrap();
        let url = link.to_url().unwrap();
        assert!(url.starts_with(URL_PREFIX));
        let parsed = InviteLink::from_url(&url, 1_100).unwrap();
        assert_eq!(parsed.body.card.body.identity, inviter.public());
        assert_eq!(parsed.body.card.body.onion, "inviter.onion");
        assert_eq!(parsed.body.key_package, vec![9u8; 4]);
        // Tamper: a different identity's signature must fail.
        let attacker = IdentityKey::generate().unwrap();
        let bad_card = crate::contact::ContactCard::sign(&attacker, "evil.onion".into(), vec![], 1, 86_400, 1_000).unwrap();
        assert!(InviteLink::generate(&inviter, bad_card, vec![9u8; 4], [7u8; 32], 600, 1_000).is_err(),
            "generate must reject a card whose identity != inviter");
    }
```

> **VERIFY:** match the real `URL_PREFIX` const + `IdentityKey`/`ContactCard` import paths used by existing tests in the file. If other invite tests assert `body.identity`/`body.onion`, update them to `body.card.body.*`.

- [ ] **Step 8: Build + run the invite tests + the wider crate**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness invite:: && cargo build --workspace --exclude skattr-ui --features test-harness`
Expected: invite tests pass; the workspace builds (proving `create_invite`/`add_contact` compile against the new format).

- [ ] **Step 9: Clippy**

Run: `. "$HOME/.cargo/env" && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/invite/link.rs crates/core/src/daemon/dispatch.rs
git commit -m "feat(invite): embed inviter's signed ContactCard in the invite (ADR 0008)

InviteLinkBody carries the inviter's self-card (identity + onion + mailboxes +
version) instead of a bare identity+onion, so the invitee can persist it and
the dialer resolves the inviter's onion via latest_card. create_invite builds
the self-card; add_contact reads card.body.*. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: `AddContact` persists the inviter's card (dialer-resolvable onion)

**Why:** With the card now in the invite (Task 1), the invitee must persist it so `latest_card(inviter)` resolves the dialer's onion.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (`add_contact`)
- Test: `crates/core/src/daemon/dispatch.rs` (the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Persist the inviter's card in `add_contact`**

In `add_contact`, after `contact_repo.upsert(&contact)` + `set_group_id` (the contact row must exist first — `put_card` requires it), add:

```rust
    // Persist the inviter's signed card so the dialer can resolve their onion
    // (the invite embeds it; ADR 0008).
    contact_repo.put_card(&link.body.card).map_err(map_err)?;
```

> **VERIFY:** `put_card` returns `Result<()>`; `map_err` converts `CoreError` → `IpcError`. It must come AFTER `upsert` (the contact-exists precondition) and BEFORE `send_welcome` (so a card-persist failure aborts cleanly with no Welcome sent).

- [ ] **Step 2: Write a test — AddContact makes the inviter dialer-resolvable**

Add to the `tests` module in `dispatch.rs`. Build a real invite for a fake "inviter" identity (sign a card + a KeyPackage the way `create_invite` does — or construct the `InviteLink` directly), run `add_contact` via `execute_command`, and assert `ContactRepo::latest_card(inviter).body.onion` is the card onion.

```rust
    #[tokio::test]
    async fn add_contact_persists_inviter_card_for_dialer() {
        use crate::contact::ContactCard;
        use crate::invite::InviteLink;
        use crate::mls::key_package::KeyPackage;
        use crate::mls::provider::MlsProvider;
        use crate::storage::{ContactRepo, KeyPackageRepo};

        let handle = test_handle();

        // The "invitee" (this daemon) needs a KeyPackage from the inviter to
        // build the group; generate one for a fake inviter identity.
        let inviter = crate::identity::IdentityKey::generate().unwrap();
        let inviter_card =
            ContactCard::sign(&inviter, "inviter.onion".into(), vec![], 1, 86_400, now_secs()).unwrap();
        let provider = MlsProvider::new();
        let kp = KeyPackage::generate(&inviter, &provider, &KeyPackageRepo::new(&handle.pool)).unwrap();
        let kp_bytes = kp.to_bytes().unwrap();
        let link = InviteLink::generate(&inviter, inviter_card, kp_bytes, [3u8; 32], 600, now_secs()).unwrap();
        let url = link.to_url().unwrap();

        execute_command(handle.clone(), Command::AddContact { invite_url: url }).await.unwrap();

        let card = ContactRepo::new(&handle.pool)
            .latest_card(&inviter.public())
            .unwrap()
            .expect("inviter card persisted");
        assert_eq!(card.body.onion, "inviter.onion");
    }
```

> **VERIFY:** the real `test_handle()` shape (does it give `handle.pool`, and is `execute_command(handle.clone(), …)` the right call? mirror the Phase 1A `send_message_with_contact_card_update_kind_is_rejected_not_panic` test in this same module). `now_secs()` — use the real clock helper the module uses (`crate::daemon::clock::now_unix_seconds()`), inline it if no `now_secs()` exists. `KeyPackage::generate`/`to_bytes`, `ContactCard::sign` signatures are as quoted in the plan. If `add_contact` requires Tor-ready for `send_welcome`, note that `send_welcome` is `await`ed but its delivery is best-effort — the test only asserts the card persisted, which happens before the send; if `send_welcome` errors hard, adjust to assert the card persisted regardless (the put_card is before send_welcome).

- [ ] **Step 3: Run the test → PASS; clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon::dispatch::tests::add_contact_persists_inviter_card_for_dialer && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "feat(dispatch): AddContact persists the inviter's card (dialer onion)

put_card the invite-embedded inviter card so latest_card resolves the
inviter's onion for the outbound dialer. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Send the invitee's self-card to the new contact (reverse-direction onion)

**Why:** After joining, the inviter has the invitee's identity (from the Welcome, ADR 0007) but no onion. The invitee must send its own self-card so the inviter can dial back. Extract the per-contact "build envelope → encrypt → save → send" out of `publish_self_card_update` into a reusable helper, and call it once (targeted) from `add_contact`.

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs`
- Test: `crates/core/src/daemon/dispatch.rs` (tests)

- [ ] **Step 1: Extract two helpers**

In `dispatch.rs`, add:

```rust
/// Build this daemon's current signed self-card (onion + reachable mailboxes).
fn build_self_card<S>(handle: &Arc<DaemonHandle<S>>) -> std::result::Result<crate::contact::ContactCard, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    let onion = handle.onion().ok_or(IpcError::Daemon(DaemonErrorKind::TorNotReady))?;
    let mailboxes: Vec<String> = crate::storage::MailboxRepo::new(&handle.pool)
        .list_mine()
        .map_err(map_err)?
        .into_iter()
        .filter(|r| r.status == crate::storage::MailboxStatus::Reachable)
        .map(|r| r.onion)
        .collect();
    crate::contact::self_card::build_next_self_card(
        &handle.pool,
        &handle.identity,
        onion,
        mailboxes,
        crate::contact::self_card::DEFAULT_TTL_SECS,
        crate::daemon::clock::now_unix_seconds(),
    )
    .map_err(map_err)
}

/// Encrypt `card` as a `ContactCardUpdate` for `peer`'s group and hand it to
/// the hub. Best-effort: load/encrypt/save failures are logged and skipped
/// (caller decides whether that's fatal). Returns true if handed to the hub.
async fn send_card_to_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    card: &crate::contact::ContactCard,
    peer: crate::identity::PublicKey,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::envelope::{Envelope, Kind, MessageId};
    use crate::mls::group::{Group, GroupId};
    use crate::storage::{ContactRepo, MlsGroupRepo};

    let group_id_bytes = match ContactRepo::new(&handle.pool).get_group_id(&peer) {
        Ok(Some(gid)) if !gid.is_empty() => gid,
        _ => return false,
    };
    let group_repo = MlsGroupRepo::new(&handle.pool);
    let mut group = match Group::load(&GroupId(group_id_bytes), &group_repo) {
        Ok(Some(g)) => g,
        _ => return false,
    };
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let msg_id = MessageId::generate();
    let env = Envelope {
        v: 1,
        id: msg_id,
        ts: now_ms,
        reply_to: None,
        kind: Kind::ContactCardUpdate { card: Box::new(card.clone()) },
    };
    let ciphertext = match group.encrypt(&env) {
        Ok(ct) => ct,
        Err(e) => {
            tracing::warn!(err = %e, "card-send: encrypt failed; skipping");
            return false;
        }
    };
    if let Err(e) = group.save(&group_repo) {
        tracing::warn!(err = %e, "card-send: save group failed; skipping");
        return false;
    }
    let _ = handle.hub.send(peer, msg_id, ciphertext).await;
    true
}
```

Then **refactor `publish_self_card_update`** to use them: build the card via `build_self_card`, then `for contact in contacts { let _ = send_card_to_contact(&handle, &card, contact.identity).await; }`. Remove the now-duplicated inline encrypt/save/send loop body.

> **VERIFY:** the existing `publish_self_card_update` builds the card inline — replace that with `build_self_card` (same onion/mailbox/build_next_self_card calls) and the loop body with `send_card_to_contact`. Keep its `async fn … -> Result<(), IpcError>` signature + its three call sites unchanged. Confirm `DaemonHandle::onion()`, `hub.send`, `Group::load/encrypt/save`, `get_group_id` signatures as quoted.

- [ ] **Step 2: Call it from `add_contact` after the Welcome**

In `add_contact`, after `handle.hub.send_welcome(...).await`, send the invitee's self-card to the inviter:

```rust
    // Send our own card to the new contact so they learn our onion for the
    // reverse direction. Best-effort: rides the same connection after the
    // Welcome; if it fails the peer learns our onion on our next message.
    let inviter = link.body.card.body.identity;
    match build_self_card(handle) {
        Ok(self_card) => {
            let _ = send_card_to_contact(handle, &self_card, inviter).await;
        }
        Err(e) => tracing::warn!(?e, "add_contact: could not build self-card to send"),
    }
```

> **VERIFY:** `handle` is `&Arc<DaemonHandle<S>>` in `add_contact`. `tracing::warn!(?e, …)` on an `IpcError` — confirm `IpcError: Debug` and that it carries no onion/pubkey (it's a `DaemonErrorKind`/typed error; if it could embed a secret, drop `?e` for static text). This must come AFTER `send_welcome` so the inviter's group is being established first.

- [ ] **Step 3: Build + run the dispatch tests + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness daemon::dispatch && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: existing dispatch tests still pass (the `publish_self_card_update` refactor is behavior-preserving — there's an existing `rotate_onion_publishes_card_update_to_contacts` test that covers it); no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "feat(dispatch): send invitee self-card to the new contact on AddContact

Extract build_self_card + send_card_to_contact (shared with
publish_self_card_update); AddContact sends our card to the inviter after the
Welcome so they learn our onion for the reverse direction. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Welcome-send arm dials on demand

**Why:** The Welcome can't be delivered to a cold peer because the `welcome_jobs` arm never dials. Mirror the `MlsApp` arm's `ensure_conn`.

**Files:**
- Modify: `crates/core/src/delivery/peer.rs` (`welcome_jobs.recv()` arm, ~line 339)
- Test: `crates/core/src/delivery/dial.rs` or `peer.rs` tests

- [ ] **Step 1: Add dial-on-demand to the Welcome arm**

Replace the `welcome_jobs.recv()` arm so it dials before sending (mirroring the `jobs` arm):

```rust
            wj = welcome_jobs.recv() => {
                let Some(wj) = wj else { break; };
                let synthetic_id = welcome_msg_id(&wj.welcome_bytes);
                if !ensure_conn::<S>(peer, &mut conn, &dialer).await {
                    let _ = wj.ack_tx.send(Err(()));
                    continue;
                }
                let Some(c) = conn.as_mut() else {
                    let _ = wj.ack_tx.send(Err(()));
                    continue;
                };
                if c.send(Frame::MlsWelcome(wj.welcome_bytes)).await.is_err() {
                    let _ = wj.ack_tx.send(Err(()));
                    conn = None;
                    drain_pending(&mut pending);
                } else {
                    pending.insert(synthetic_id, wj.ack_tx);
                    last_traffic = tokio::time::Instant::now();
                }
            }
```

> **VERIFY:** `ensure_conn`, `dialer`, `peer`, `conn`, `drain_pending`, `pending`, `last_traffic`, `welcome_msg_id` are all in scope in `full_run` (the `jobs` arm uses them identically). NO `expect` — the second `let Some(c) = conn.as_mut() else {…}` is the no-`expect` form.

- [ ] **Step 2: Write a test — the Welcome arm dials when cold**

Mirror the existing `actor_dials_when_cold_and_delivers` test in `delivery/dial.rs` (Phase 1B), but drive a Welcome instead of an app message: build a real authenticated duplex pair via a `OneShotDialer`, submit a `WelcomeJob` to a cold actor through the hub's `send_welcome`, and assert the responder receives `Frame::MlsWelcome`.

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn welcome_arm_dials_when_cold_and_delivers() {
        use crate::delivery::hub::DeliveryHub;
        use crate::transport::{handshake_responder, Frame};
        use std::sync::Mutex as StdMutex;
        use tokio::io::DuplexStream;

        let alice = IdentityKey::generate().unwrap();
        let bob = IdentityKey::generate().unwrap();
        let bob_pub = PublicKey(bob.public().0);
        let bob_x = crate::identity::key::ed25519_pub_to_x25519(
            &ed25519_dalek::VerifyingKey::from_bytes(&bob.public().0).unwrap(),
        );
        let (a, b) = tokio::io::duplex(64 * 1024);
        let init = tokio::spawn(async move {
            crate::transport::handshake_initiator(a, &alice, &bob_x, None).await.unwrap().0
        });
        let resp = tokio::spawn(async move { handshake_responder(b, &bob, None).await.unwrap().0 });
        let alice_conn = init.await.unwrap();
        let mut bob_conn = resp.await.unwrap();

        let pool = Arc::new(Pool::in_memory());
        let dialer: Arc<dyn OutboundDial<DuplexStream>> =
            Arc::new(OneShotDialer(StdMutex::new(Some(alice_conn))));
        let hub = DeliveryHub::<DuplexStream>::new_with_dialer(pool, dialer);

        let _ack = hub.send_welcome(bob_pub, b"welcome-bytes".to_vec()).await.unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), bob_conn.recv())
            .await.expect("frame within 2s").unwrap();
        match frame {
            Some(Frame::MlsWelcome(w)) => assert_eq!(w, b"welcome-bytes"),
            other => panic!("expected MlsWelcome, got {other:?}"),
        }
    }
```

> **VERIFY:** reuse the `OneShotDialer` + imports already in `dial.rs`'s `tests` module (this test belongs next to `actor_dials_when_cold_and_delivers`). `hub.send_welcome` returns `Result<oneshot::Receiver<…>>`. If `DeliveryHub::new_with_dialer` is `#[cfg(test)]`-only (it is, from Phase 1B), this test must live in the same crate (it does). `welcome_bytes` here is opaque (no MLS validation on the wire path — the actor just frames+sends it), so arbitrary bytes are fine.

- [ ] **Step 3: Run the test + delivery suite + clippy**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-core --lib --features test-harness delivery:: && cargo clippy -p skattr-core --all-targets --features test-harness -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/delivery/peer.rs crates/core/src/delivery/dial.rs
git commit -m "feat(delivery): Welcome-send arm dials on demand

Mirror the MlsApp arm's ensure_conn in the welcome_jobs arm so the invitee
can dial a cold inviter to deliver the Welcome. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: First-contact loopback guardrail (the prize)

**Why:** Prove the whole `invite → add → Welcome → bidirectional message` flow through two real `run_with_transport` daemons over loopback — the genuine first-contact path, no seeding, no `test_exports` hub hand-wiring.

**Files:**
- Create: `crates/tests/src/first_contact_direct.rs`
- Modify: `crates/tests/src/lib.rs` (register the module)

- [ ] **Step 1: Write the guardrail**

Create `crates/tests/src/first_contact_direct.rs` (GPLv3 header). Model the daemon spawn on the Phase 1B `daemon_run_direct.rs` (two `run_loopback` daemons sharing a `LoopbackNet`, `init_vault`, `config_for`, `IpcClient`), and the invite flow on `welcome_propagation.rs` (`Command::CreateInvite` / `AddContact`). Crucially: drive the REAL invite flow (NOT the `seed_established_pair` seeding) — Alice's `create_invite` embeds her loopback onion in the card, Bob's `add_contact` persists it, and delivery happens via dial.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_contact_invite_add_then_bidirectional_over_loopback() {
    // 1. Two temp dirs; init_vault each. Shared LoopbackNet; Alice "alice.onion",
    //    Bob "bob.onion". Spawn run_loopback for each; await Ready; assert onions.
    // 2. Alice IpcClient -> Command::CreateInvite { nickname: None, ttl_secs: Some(600) }
    //    -> CommandResult::InviteCreated { url, .. }.  (The card embeds "alice.onion".)
    // 3. Bob IpcClient -> Command::AddContact { invite_url: url } -> ContactAdded.
    //    (Bob persists Alice's card -> dials "alice.onion" -> delivers Welcome ->
    //     sends Bob's card back -> Alice learns "bob.onion".)
    // 4. Poll Alice's ListContactsWithFilter (or ListContacts) for Bob's group_state
    //    == Active, bounded ~30s (Welcome landed).
    // 5. Subscribe Bob's events (EventFilter::Messages); Alice -> SendMessage(bob, Text
    //    "hello-bob"); assert Bob's MessageReceived body == "hello-bob" (bounded).
    // 6. Subscribe Alice's events; Bob -> SendMessage(alice, Text "hello-alice");
    //    assert Alice's MessageReceived body == "hello-alice" (bounded).
    // 7. Graceful shutdown both.
}
```

> **VERIFY — make this concrete by reading the two model tests:**
> - Copy the daemon-spawn scaffolding (`init_vault`, `config_for`, `run_loopback`, `LoopbackNet`, Ready handling, shutdown) verbatim from `crates/tests/src/daemon_run_direct.rs`. Do NOT call `seed_established_pair` — this test uses the real invite flow.
> - Copy the `CreateInvite`/`AddContact` IPC idiom + the contact pubkey discovery from `welcome_propagation.rs`. Get Bob's pubkey from `ContactAdded`'s `ContactSummary.pubkey`; get Alice's from the invite-created side or the `ContactAdded` summary.
> - Group-active polling: reuse `welcome_propagation.rs`'s `wait_for_alice_group_active` idiom (poll `ListContacts`/`ListContactsWithFilter`, check the contact's `group_state == Some(MlsGroupStateLabel::Active)`).
> - Message assert: reuse `daemon_run_direct.rs`'s `subscribe_messages` + `wait_for_message` helpers (subscribe BEFORE the send to avoid the event race documented there).
> - Bound every await with `tokio::time::timeout` so a regression fails fast.
> - This test is the 1C exit criterion. If it surfaces a real production gap (not test wiring), report DONE_WITH_CONCERNS with specifics — do NOT weaken assertions.

- [ ] **Step 2: Register the module**

In `crates/tests/src/lib.rs`, add `mod first_contact_direct;` next to the other `mod` lines.

- [ ] **Step 3: Run the guardrail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests first_contact_invite_add_then_bidirectional_over_loopback -- --nocapture`
Expected: PASS — Alice's group goes Active and both directions deliver.

- [ ] **Step 4: Full gates**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo test -p skattr-tests first_contact_invite_add_then_bidirectional_over_loopback && \
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings && \
cargo fmt --all -- --check
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/tests/src/first_contact_direct.rs crates/tests/src/lib.rs
git commit -m "test(guardrail): first-contact invite->add->bidirectional over loopback

Two run_with_transport daemons complete the real invite flow (CreateInvite ->
AddContact -> Welcome delivered via dial -> bidirectional messages) over an
in-process loopback, no Tor, no test_exports hub hand-wiring. The Phase 1C
exit criterion. (T0-1)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Final verification

- [ ] **Run the full gates**

Run:
```bash
. "$HOME/.cargo/env" && \
cargo fmt --all -- --check && \
cargo test -p skattr-core --features test-harness && \
cargo test -p skattr-tests && \
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings && \
cargo build -p skattr-cli
```
Expected: all green; CLI builds.

- [ ] **Confirm the behaviors by reading the diff**
  - The invite embeds the inviter's signed card; round-trips (Task 1).
  - `AddContact` persists the inviter's card (dialer-resolvable) and sends the invitee's card back (Tasks 2–3).
  - The Welcome-send arm dials on demand (Task 4).
  - Two daemons complete the real first-contact flow bidirectionally (Task 5).

---

## Spec coverage (self-review)

| Spec / ADR item | Covered by | Notes |
|---|---|---|
| ADR 0008 invite embeds card | Task 1 | struct + generate + to_url + from_url + round-trip test |
| create_invite builds + embeds self-card | Task 1 (Step 5) | mailbox idiom from publish_self_card_update |
| AddContact persists inviter card | Task 2 | put_card; latest_card-resolves test |
| AddContact sends invitee self-card | Task 3 | shared helpers; targeted send |
| Welcome-arm dial-on-demand | Task 4 | mirrors MlsApp arm; cold-dial test |
| First-contact guardrail | Task 5 | real invite flow over loopback |
| Keep 1B seeded guardrail | — | untouched (still passes) |
| welcome_propagation new format | — | wire-level (Command-driven); compiles unchanged; now functional over real Tor |
| Out of scope (mailbox fallback, h_transport) | — | Phase 2 |

The wire-format change (ADR 0008) is the only protocol edit; `ContactCardUpdate`/`ContactCardReceived` are reused; no new `Command`/`CommandResult`/`Event`/`Frame` variants.
