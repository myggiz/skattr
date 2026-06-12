# Phase 1C — First Contact Over Direct Transport (Design)

**Date:** 2026-06-12
**Status:** Approved design; implementation plan to follow.
**Roadmap:** `docs/superpowers/specs/2026-06-12-v1.0-roadmap.md` (Phase 1, "first
contact" deferral recorded under the Phase 1 outcome).
**Predecessors:** Phase 1B (direct-transport assembly, merged); ADR 0007
(Welcome carve-out + transport↔MLS binding). **New:** ADR 0008 (invite embeds
the inviter's signed ContactCard).

---

## 1. Problem

Phase 1B wired the direct-transport assembly (outbound dialer + inbound accept
loop + Welcome carve-out) and proved bidirectional delivery between *established*
contacts. Its guardrail exposed that the **full first-contact flow**
(`invite → add → Welcome delivery → reverse message`) does **not** yet work,
because of three gaps (verified in code, 2026-06-12):

- **(a) The Welcome-send arm doesn't dial on demand.** `delivery/peer.rs`'s
  `welcome_jobs.recv()` arm only sends if `conn.is_some()`, else `ack Err` — it
  never calls `ensure_conn` (the dial-on-demand the `jobs`/`MlsApp` arm has). So
  the invitee can't dial the inviter to deliver the Welcome.
- **(b) The invitee can't resolve the inviter's onion.** The dialer resolves a
  peer's onion *only* from a signed `ContactCard` (`latest_card`), but
  `AddContact` persists the inviter contact with `card: None` and stores the
  inviter's (signed, invite-borne) onion nowhere the dialer reads.
- **(c) The inviter never learns the invitee's onion.** `AddContact` does not
  send the invitee's self-card to the inviter, so after joining, the inviter has
  the invitee's identity (from the Welcome, ADR 0007) but no onion to dial back.

## 2. Goal & scope

**Goal:** two real daemons complete `invite → add → Welcome → bidirectional
message` over direct transport, proven by a non-`#[ignore]` CI guardrail driving
the real `run_with_transport` assembly (not seeded state, not `test_exports` hub
hand-wiring).

### In scope
1. **(ADR 0008) Invite embeds the inviter's signed ContactCard** — the onion
   bootstrap (replaces the bare `identity`/`onion` fields).
2. **`AddContact` persists the inviter's card** (`put_card`) and **sends the
   invitee's self-card to the new contact** after the Welcome.
3. **Welcome-arm dial-on-demand** (mirror the `MlsApp` arm's `ensure_conn`).
4. **The first-contact guardrail** — a new live loopback test of the real
   invite flow, plus updates to the existing `#[ignore]` real-Tor twins.

### Out of scope (unchanged deferrals)
- Mailbox fallback for Welcome / messages (Task 2.E.5 / T1-6) — **Phase 2**.
- `h_transport` MLS-PSK binding (T1-1) — **Phase 2** (ADR 0007's identity binding
  is the lighter, complementary check already in place).
- Onion-key rotation (Task 23.5); multi-member groups.

### Non-goals
No change to the Noise pattern, MLS ciphersuite, or the message wire format
beyond the InviteLink body (ADR 0008). No new `Command`/`Event`/`Frame` variants.

## 3. Components & changes

### 3.1 Invite embeds the inviter's card (ADR 0008)
`invite/link.rs`:
```rust
pub struct InviteLinkBody {
    pub card: ContactCard,   // inviter's signed self-card (identity + onion + …)
    pub key_package: Vec<u8>,
    pub psk: [u8; 32],
    pub expires_at: i64,
}
```
- `InviteLink::generate(inviter, card, key_package, psk, ttl, now)` — takes the
  inviter's self-card (built by the caller via `build_next_self_card`) and signs
  the whole body. The body's existing Ed25519 signature binds card+KP+PSK.
- `InviteLink::from_url` verification unchanged in shape (verify the link
  signature over the body); additionally the embedded card is independently
  verifiable.
- Update every reader of `link.body.identity` / `link.body.onion` to
  `link.body.card.body.identity` / `link.body.card.body.onion` (AddContact,
  ContactSummary, any tests).

### 3.2 `create_invite` builds + embeds the self-card
`daemon/dispatch.rs` invite-creation handler: read the daemon onion (already
required), gather reachable mailboxes (as `publish_self_card_update` does), build
the inviter's self-card via `contact::self_card::build_next_self_card`, and pass
it to `InviteLink::generate`. (This consumes one self-card version bump per
invite — acceptable; the self-card version is monotonic and advisory.)

### 3.3 `AddContact` — persist inviter card + send self-card to the new contact
`daemon/dispatch.rs::add_contact`, after the existing `upsert(contact)` +
`set_group_id`:
1. `ContactRepo::new(&pool).put_card(&link.body.card)` — persists the inviter's
   signed card so `latest_card(inviter)` resolves the dialer's onion. (The
   contact row already exists from `upsert`, satisfying `put_card`'s
   contact-exists precondition.)
2. After `hub.send_welcome(inviter, welcome)`, **send the invitee's own current
   self-card to just this new contact** — build the self-card, MLS-encrypt a
   `Kind::ContactCardUpdate` for this group, and `hub.send(inviter, …)`. A
   **targeted** send (not the broadcast `publish_self_card_update`, which would
   re-send to every existing contact and bump the version once globally). Extract
   the per-contact encrypt-and-send into a helper reused by both the broadcast
   and the targeted path (DRY).

Ordering: the Welcome and the card both ride the invitee's peer-actor connection
to the inviter (same connection, sequential frames), so the inviter processes the
Welcome first (group → Active, via ADR 0007 carve-out), then the
`ContactCardUpdate` (→ `put_card`, inviter learns the invitee's onion).

### 3.4 Welcome-arm dial-on-demand
`delivery/peer.rs` `welcome_jobs.recv()` arm: call
`ensure_conn(peer, &mut conn, &dialer).await` before sending `Frame::MlsWelcome`
(exactly as the `jobs` arm does), `ack Err` + `continue`/drop on dial failure.
This lets the invitee dial the inviter (now resolvable via §3.3 step 1) to
deliver the Welcome.

## 4. Data flow (the full first-contact round trip)

1. **Alice `create_invite`** → invite embeds Alice's signed self-card (onion).
2. **Bob `add_contact(invite)`** → persists Alice's card (dialer can now resolve
   Alice) → creates the 2-member group → `send_welcome`: the Welcome-arm
   **dials Alice** (card onion) and delivers `Frame::MlsWelcome` → then sends
   **Bob's self-card** (`ContactCardUpdate`) over the same connection.
3. **Alice** (accept-loop carve-out, ADR 0007): accepts + binds + ingests Bob;
   `dispatch_welcome_bootstrap` joins the group (→ Active) and persists Bob (his
   identity). The next frame, Bob's `ContactCardUpdate`, is dispatched →
   `put_card(bob)` → **Alice now has Bob's onion**.
4. Both sides now resolve each other's onion → **`Alice→Bob` and `Bob→Alice`
   messages dial + deliver** through the production assembly.

## 5. Error handling

- **Welcome dial failure** → `welcome_jobs` arm `ack Err` → `AddContact`'s
  `send_welcome` ack resolves to failure; surface to the caller (the existing
  WelcomeJob ack path) so the UI/CLI can report "invite added but Welcome not yet
  delivered"; retried on the next outbound attempt to that peer.
- **Invitee card-send failure** (step 2's targeted send) → non-fatal; the inviter
  still has the invitee's *identity* (from the Welcome) and learns the onion on
  the invitee's next message or a later card rotation. Log, don't fail `AddContact`.
- **Card-persist failure** (`put_card`) → abort `AddContact` with the error before
  `send_welcome` (no half-established contact). `put_card`'s monotonic-version
  guard and contact-exists check are unchanged.
- **Malformed invite** (card identity mismatch, bad signature, expired) → rejected
  in `from_url` / a cross-check in `add_contact`, as today.

## 6. Testing

- **First-contact guardrail (new, non-`#[ignore]`, no Tor)** in
  `crates/tests/src/`: two `run_with_transport` daemons over a shared
  `LoopbackTransport`, driving the **real** IPC flow — Alice `CreateInvite` → Bob
  `AddContact(invite)` → assert Alice's group reaches `Active` (Welcome delivered
  via dial) → `Alice→Bob` text delivered → `Bob→Alice` text delivered — all via
  `Event::MessageReceived`, bounded timeouts. This is the 1C prize: it would fail
  if any of (a)/(b)/(c) regressed.
- **Keep** the 1B seeded-established guardrail (steady-state delivery check).
- **Unit tests:** InviteLink round-trip with embedded card (generate → to_url →
  from_url → verify, card identity/onion intact); `AddContact` persists the
  inviter card (`latest_card` returns it post-add); the Welcome-arm dials when
  cold (extend the existing dial-on-demand test pattern to the welcome path).
- **Update** `welcome_propagation` (it drives `CreateInvite`/`AddContact`, so it
  touches the new invite format) — it stays `#[ignore]` for real-Tor fidelity but
  must compile against the new format. The `daemon_run_direct` real-Tor twin
  seeds *established* contacts (no invite flow), so it needs no invite-format
  change. The new loopback first-contact guardrail is the CI guardrail; the
  real-Tor tests remain `#[ignore]`.

## 7. Exit criteria

- The first-contact loopback guardrail passes in CI: two real daemons complete
  `invite → add → Welcome → bidirectional message` with no Tor and no
  `test_exports` hub hand-wiring.
- `InviteLink` round-trips the embedded card; `AddContact` makes the inviter
  dialer-resolvable and the inviter learns the invitee's onion.
- `cargo fmt --check`, `cargo clippy --workspace --exclude skattr-ui
  --all-targets --features test-harness -- -D warnings`, and the full
  non-ignored suite are green; `cargo build -p skattr-cli` builds.

## 8. Module-visibility & wire notes

Changes stay within existing modules (`invite`, `contact`, `daemon/dispatch`,
`delivery/peer`); no widening of `core`'s public API beyond what already exists
(`invite` is public). The **only** wire-format change is the `InviteLinkBody`
(ADR 0008); `ContactCardUpdate`/`ContactCardReceived` are reused unchanged; no
new `Command`/`CommandResult`/`Event`/`Frame` variants. The
`wire_format_append_only` snapshot test covers `Command`/`CommandResult`; the
InviteLink change is a deliberate, ADR-backed edit outside that snapshot.
