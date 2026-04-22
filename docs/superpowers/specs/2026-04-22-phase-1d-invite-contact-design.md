# Phase 1.D — Invite & Contact Flow Design

**Status:** Approved 2026-04-22. Sub-project 1.D of the Phase 1 decomposition (`2026-04-21-phase-1-decomposition.md`). Depends on 1.C (KeyPackage + `KeyPackageRepo`), transitively on 0.D storage.

## Goal

Turn the out-of-band invite flow into working code: Alice mints a signed `skattr://invite/v1#...` URL containing her onion, her Ed25519 identity, a single-use MLS KeyPackage, a 32-byte PSK, and an expiry; Bob parses + verifies the URL, records the received KeyPackage in his local `KeyPackageRepo`, and (later, when 1.F wires the full flow) flips `consumed=1` after a successful MLS join. Alongside: `ContactCard::{sign, verify}` with monotonic version semantics persisted through `storage::contacts`. 1.D closes the gap between 1.C (we have a group abstraction) and 1.F (we drive it over real Tor) by shipping the exchange layer.

## Scope

**In scope**

- `invite::link::InviteLink`: `generate` / `from_url(now)` / `to_url` / `is_consumed` / `record_received` / `mark_consumed` / `kp_hash`. Split into `InviteLinkBody` (signed content) + `InviteLink` (body + signature + `InvitePsk` guard).
- Invite wire format per design spec §1.4: `skattr://invite/v1#id=<base32>&onion=<56char>&kp=<base64url>&psk=<base64url>&exp=<i64>&sig=<base64url>`.
- Canonical CBOR (ciborium) over `InviteLinkBody` is the signed form.
- Single-use enforcement: reuse `KeyPackageRepo.consumed`, keyed by SHA-256 of the KP bytes.
- `contact::card::ContactCard`: `sign` / `verify(now)`. Split into `ContactCardBody` + `ContactCard`.
- `storage::ContactRepo`: `put_card(&ContactCard)` rejects stale versions; `latest_card(&PublicKey)` returns the highest-version row; `get` / `list` hydrate `Contact.card`.
- Migration `0003_contact_cards.sql`: new `contact_cards` table keyed by `(contact_id, version)`; `schema_version` bumps 2→3.
- QR SVG rendering via the `qrcode` crate (feature-gated `qr`).
- Error taxonomy funnelled through `CoreError::Invite(String)` and `CoreError::Contact(String)` with fixed prefixes.
- Inline unit tests + one integration test (`crates/tests/src/invite_roundtrip.rs`, gated on `test-harness`).

**Out of scope**

- **PNG QR rendering.** Delete the `render_png` stub. UI-side PNG needs land with Phase 2's Tauri layer.
- **Onion rotation.** `contact/rotation.rs` stays as a `todo!()` stub. Rotation requires grace-period bookkeeping + the outbox (1.E) to broadcast new cards — out of 1.D's scope and not in the decomposition exit criterion.
- **Mailbox list semantics.** `ContactCardBody.mailboxes: Vec<String>` stays empty in 1.D; 1.E populates it when the mailbox client lands.
- **Driving the invite through MLS.** 1.F wires the full "Alice receives Bob's invite → Noise dial → MLS add_member → mark invite consumed" flow. 1.D ships the primitives; 1.F composes them.
- **Contact card broadcasting / sync.** Each peer publishes their card out-of-band for 1.D. 1.E introduces automatic card exchange inside the authenticated channel.
- **Invite PSK lookup by peer identity.** Deferred from 1.B and still deferred here — the `AuthenticatedConnection` layer's PSK argument is set by the caller who already parsed the invite.

## Locked decisions (settled during brainstorming)

| Decision | Choice |
|---|---|
| Single-use enforcement | (A) Reuse `KeyPackageRepo.consumed` keyed by SHA-256 of KP bytes. No new `invite_usage` table. |
| Canonical signed form | (A) Canonical CBOR via ciborium over a dedicated `*Body` struct that excludes the signature. Same helper used by both invite and card. |
| Body/wrapper split | (A1) Two types per signable thing: `InviteLinkBody` + `InviteLink`, `ContactCardBody` + `ContactCard`. Signing consumes a body; verifying yields the body's `identity`. |
| ContactCard storage | (A) New `contact_cards` table keyed by `(contact_id, version)`. Full history. Migration 0003. |
| Expiry check | Inline in `from_url(url, now)` and `ContactCard::verify(now)`. Explicit `now: i64` argument for hermetic tests. |
| QR scope | SVG only. `render_png` stub deleted. |
| Rotation scope | Entirely deferred to Phase 2. `contact/rotation.rs` untouched. |
| Invite field encoding | `id` base32 (RFC 4648 lowercase, unpadded), `kp`/`psk`/`sig` base64url (NO_PAD), `onion` as the 56-char base32 literal, `exp` as decimal ASCII i64. Matches design doc §1.4. |

## Architecture

All changes inside `crates/core/src/` plus one migration file and one test-harness export.

```
invite/mod.rs                MODIFY: re-export InviteLink + InvitePsk + helpers
invite/link.rs               REWRITE: InviteLinkBody + InviteLink with canonical-CBOR
                                       signing, URL parse/build, single-use helpers,
                                       kp_hash; inline tests
invite/qr.rs                 REWRITE: render_svg only; render_png deleted; inline tests

contact/mod.rs               NO CHANGE
contact/contact.rs           NO CHANGE
contact/card.rs              REWRITE: ContactCardBody + ContactCard with sign/verify(now);
                                       inline tests
contact/rotation.rs          NO CHANGE (stub stays)

storage/contacts.rs          MODIFY: put_card / latest_card; hydrate Contact.card in
                                     get/list; extend tests for cards + stale-version
                                     rejection + cascade-delete
storage/migrations/
  0003_contact_cards.sql     NEW: contact_cards table; schema_version++
storage/migrations.rs        MODIFY: register migration 0003; extend expected-tables
                                     assertion to include contact_cards
storage/mod.rs               NO CHANGE (ContactRepo already re-exported)

error.rs                     NO CHANGE: reuse CoreError::Invite(String) and
                                        CoreError::Contact(String)

lib.rs                       MODIFY: test_exports += InviteLink, ContactCard,
                                     ContactRepo

crates/tests/src/invite_roundtrip.rs   NEW: Alice-generates → Bob-parses-verifies-records
                                             → consumed flag flow
crates/tests/src/lib.rs      MODIFY: declare invite_roundtrip module
```

No workspace `Cargo.toml` edits — all deps (`base32`, `base64`, `ciborium`, `qrcode`, `sha2`, `zeroize`) are already in place.

## Key types

### `InviteLinkBody` + `InviteLink`

```rust
// invite/link.rs
use serde::{Deserialize, Serialize};

/// Content that the inviter signs. Deliberately excludes the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteLinkBody {
    /// Inviter's long-term Ed25519 identity.
    pub identity: PublicKey,
    /// Onion service to dial for first contact.
    pub onion: String,
    /// Single-use MLS KeyPackage (binary).
    #[serde(with = "serde_bytes")]
    pub key_package: Vec<u8>,
    /// 32-byte one-time secret mixed into the Noise PSK and the first MLS Commit.
    pub psk: [u8; 32],
    /// Unix timestamp (seconds) after which the invite is invalid.
    pub expires_at: i64,
}

/// Parsed + verified invite link. The only way to obtain one is via
/// `generate` or `from_url(url, now)`, both of which verify the
/// signature before returning.
pub struct InviteLink {
    /// Unsigned body fields. `psk` has been zeroized after copying into
    /// the [`InvitePsk`] guard; read the PSK via `self.psk` (the
    /// Zeroizing wrapper), not `self.body.psk`.
    pub body: InviteLinkBody,
    /// Ed25519 signature over canonical CBOR of `body`.
    pub signature: Signature,
    /// Zeroizing copy of the PSK. Dropped on `InviteLink` drop.
    pub psk: InvitePsk,
}

/// A 32-byte one-time secret embedded in an invite.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct InvitePsk(pub [u8; 32]);

impl InviteLink {
    /// Build + sign a new invite.
    pub fn generate(
        inviter: &IdentityKey,
        onion: String,
        key_package: Vec<u8>,
        psk: [u8; 32],
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self>;

    /// Parse + verify a `skattr://invite/v1#...` URL. Validates:
    /// - scheme prefix
    /// - all six required params present
    /// - each param decodes cleanly
    /// - Ed25519 signature verifies against body.identity
    /// - `now <= body.expires_at`
    pub fn from_url(url: &str, now: i64) -> Result<Self>;

    /// Re-serialize to a URL. Deterministic: two calls on the same
    /// InviteLink yield the same string.
    pub fn to_url(&self) -> Result<String>;

    /// SHA-256 of `body.key_package` — the storage key for single-use
    /// tracking in `KeyPackageRepo`.
    pub fn kp_hash(&self) -> [u8; 32];

    /// Record this received invite's KP under `direction='theirs'` in
    /// the provided repo. Idempotent (no-op if already recorded).
    pub fn record_received(&self, kp_repo: &KeyPackageRepo) -> Result<()>;

    /// Whether this invite's KP has been marked consumed in the repo.
    pub fn is_consumed(&self, kp_repo: &KeyPackageRepo) -> Result<bool>;

    /// Flip `consumed=1` for this invite's KP. Called by the caller
    /// after a successful MLS join completes. Idempotent.
    pub fn mark_consumed(&self, kp_repo: &KeyPackageRepo) -> Result<()>;
}
```

### `ContactCardBody` + `ContactCard`

```rust
// contact/card.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCardBody {
    pub identity: PublicKey,
    pub onion: String,
    /// Mailbox onion addresses. Empty in 1.D; 1.E populates.
    pub mailboxes: Vec<String>,
    /// Monotonic version number. Higher is newer.
    pub version: u64,
    /// Unix timestamp (seconds) after which the card is stale.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactCard {
    pub body: ContactCardBody,
    pub signature: Signature,
}

impl ContactCard {
    pub fn sign(
        signer: &IdentityKey,
        onion: String,
        mailboxes: Vec<String>,
        version: u64,
        ttl_secs: u64,
        now: i64,
    ) -> Result<Self>;

    /// Verify the Ed25519 signature over the body. On success returns
    /// the body's `identity` (caller typically compares against a known
    /// pubkey). Also checks `now <= body.expires_at`.
    pub fn verify(&self, now: i64) -> Result<PublicKey>;
}
```

### `ContactRepo` additions

```rust
// storage/contacts.rs
impl<'p> ContactRepo<'p> {
    /// Insert a freshly-verified ContactCard. Rejects with
    /// `CoreError::Contact("contact: card: stale version")` if the
    /// version is not strictly greater than the latest stored card's
    /// version for the same identity. Contact row must exist (FK).
    pub fn put_card(&self, card: &ContactCard) -> Result<()>;

    /// Return the highest-version card for an identity, or None.
    pub fn latest_card(&self, identity: &PublicKey) -> Result<Option<ContactCard>>;

    // get / list now hydrate Contact.card from latest_card().
}
```

### Migration 0003

```sql
-- skattr schema migration 0003: contact cards
UPDATE schema_version SET version = 3 WHERE version = 2;

CREATE TABLE IF NOT EXISTS contact_cards (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    card_blob BLOB NOT NULL,
    verified_at INTEGER NOT NULL,
    UNIQUE (contact_id, version)
);

CREATE INDEX IF NOT EXISTS idx_contact_cards_latest
  ON contact_cards(contact_id, version DESC);
```

## Wire format

### Invite URL

```
skattr://invite/v1#id=<BASE32_PK>&onion=<ONION_B32>&kp=<BASE64URL_KP>&psk=<BASE64URL_PSK>&exp=<I64>&sig=<BASE64URL_SIG>
```

- `BASE32_PK`: RFC 4648 base32, lowercase, no padding. 32 bytes → 52 chars.
- `ONION_B32`: 56-char base32 Tor v3 onion address as-is.
- `BASE64URL_*`: URL-safe base64, no padding. Used for the KP bytes (variable length), the 32-byte PSK (43 chars), and the 64-byte signature (86 chars).
- `I64`: decimal ASCII.

Field order in the URL is fixed: `id`, `onion`, `kp`, `psk`, `exp`, `sig`. Parsers accept any order (matches how URL fragments are typically parsed), but `to_url` always emits the fixed order so `to_url → from_url → to_url` is idempotent.

### Signed form (invite)

CBOR-encode the `InviteLinkBody` struct with `ciborium::ser::into_writer`. Field order: `identity`, `onion`, `key_package`, `psk`, `expires_at`. Sign the resulting bytes with the inviter's Ed25519 key.

### Signed form (contact card)

CBOR-encode the `ContactCardBody` struct the same way. Field order: `identity`, `onion`, `mailboxes`, `version`, `expires_at`.

## Error surface

| Condition | Surfaced as |
|---|---|
| URL doesn't start with `skattr://invite/v1#` | `CoreError::Invite("invite: unsupported scheme")` |
| Fragment missing a required param | `CoreError::Invite("invite: missing field {name}")` |
| Base32/base64url decode failure | `CoreError::Invite("invite: malformed {field}")` |
| Unsupported signature length | `CoreError::Invite("invite: malformed sig")` (hit via the malformed path) |
| Ciborium fails to encode the body | `CoreError::Invite("invite: cbor encode: {detail}")` |
| Signature verification fails | `CoreError::Invite("invite: signature verification failed")` |
| `now > body.expires_at` | `CoreError::Invite("invite: expired")` |
| `mark_consumed` on unrecorded hash | `CoreError::Invite("invite: unknown: not recorded")` |
| ContactCard body deserialization fails | `CoreError::Contact("contact: card: malformed")` |
| ContactCard signature verification fails | `CoreError::Contact("contact: card: signature verification failed")` |
| ContactCard `now > expires_at` | `CoreError::Contact("contact: card: expired")` |
| ContactCard `version <= latest stored` | `CoreError::Contact("contact: card: stale version")` |
| `put_card` but contact row doesn't exist | `CoreError::Contact("contact: card: contact not found")` |

All strings are fixed, parameterised only by field names or low-detail context. No key bytes, no payload bytes, no raw CBOR errors.

## Testing strategy

### Unit tests

**`invite::link::tests`:**
1. `generate_then_to_url_then_from_url_round_trip` — Alice signs, serializes, re-parses; fields match.
2. `to_url_is_deterministic` — two calls produce identical strings.
3. `from_url_rejects_tampered_signature` — flip one bit of `sig`, parse fails with "signature verification failed".
4. `from_url_rejects_expired` — `now = expires_at + 1`, fail with "expired".
5. `from_url_rejects_unsupported_scheme` — `https://example.com/whatever`, fail with "unsupported scheme".
6. `from_url_rejects_missing_field` — drop `&kp=...`, fail with "missing field kp".
7. `from_url_rejects_malformed_base32_id` — `id=not-base32-at-all!!`, fail with "malformed id".
8. `kp_hash_is_sha256_of_key_package` — verify against a hand-computed SHA-256 for a fixed KP byte pattern.
9. `record_received_then_is_consumed_false_then_mark_consumed_then_is_consumed_true`.
10. `record_received_is_idempotent` — call twice, no error, single row.
11. `mark_consumed_on_unrecorded_errors` — call without record first, fail with "invite: unknown".
12. `psk_is_zeroized_in_body_after_parse` — the `body.psk` field is `[0u8; 32]` after `from_url` moves into the guard.

**`invite::qr::tests`** (feature-gated):
13. `render_svg_contains_url_in_some_form` — SVG output is non-empty and Base64-encodes at the expected size for a standard invite.

**`contact::card::tests`:**
14. `sign_then_verify_returns_identity` — ContactCard round-trip; verify returns the correct pubkey.
15. `verify_rejects_tampered_signature` — flip `signature.0[0]`, fail.
16. `verify_rejects_expired` — `now > expires_at`, fail with "expired".
17. `verify_rejects_wrong_signer` — same body, signed by a different `IdentityKey`; verify fails.

**`storage::contacts::tests`** (extend existing):
18. `put_card_then_latest_card_round_trip`.
19. `put_card_rejects_stale_version` — v1, then v1 again, and v0 — both fail with "stale version".
20. `put_card_rejects_when_contact_absent` — insert card for pubkey with no contact row.
21. `get_hydrates_latest_card` — upsert contact, put_card v1, put_card v2, get returns contact with `card.body.version == 2`.
22. `cascade_delete_removes_cards` — upsert, put_card, remove contact → `contact_cards` row count is 0.

### Integration test

`crates/tests/src/invite_roundtrip.rs`, gated on `#[cfg(feature = "test-harness")]`:

**`alice_mints_invite_bob_parses_records_and_consumes`:**
1. Alice: `IdentityKey::generate`; `KeyPackage::generate(&alice_id, provider, &alice_kp_repo)`.
2. Alice: `InviteLink::generate(&alice_id, "aaaa...aaaa".into(), kp_bytes, [0xAA; 32], 3600, now=1_000_000)`.
3. Alice: `let url = invite.to_url()?;`.
4. Bob: `let parsed = InviteLink::from_url(&url, now=1_000_010)?;` — well within TTL.
5. Bob: `parsed.record_received(&bob_kp_repo)?;` then `assert!(!parsed.is_consumed(&bob_kp_repo)?)`.
6. Bob: `parsed.mark_consumed(&bob_kp_repo)?;` then `assert!(parsed.is_consumed(&bob_kp_repo)?)`.
7. Replay attempt: `let reparsed = InviteLink::from_url(&url, now=1_000_020)?;` — parse succeeds (parse is stateless). `reparsed.record_received(&bob_kp_repo)?;` — idempotent. `assert!(reparsed.is_consumed(&bob_kp_repo)?)` — still true.
8. Expiry: `InviteLink::from_url(&url, now=1_001_000)` fails with "expired" (TTL was 3600).

No Tor, no Noise. `alice_id`, `bob_kp_repo` live in separate in-memory pools.

## Dependencies

All already present in the workspace:

- `base32 = "0.5"` (RFC 4648 variant in the existing API)
- `base64 = "0.22"` (URL_SAFE_NO_PAD)
- `ciborium = "0.2"` (canonical CBOR)
- `serde` / `serde_bytes`
- `sha2 = "0.10"` (KP hash)
- `qrcode` (feature-gated on `qr`)
- `zeroize = "1"` (PSK guard)
- `ed25519-dalek = "2"` (existing identity signing path)

No new third-party deps. No `Cargo.toml` edits.

## Risks

- **CBOR canonicalization drift.** `ciborium` is deterministic for struct serialization under stable Rust struct layouts, but map-key ordering and integer encoding can bite. Mitigation: a sign→verify round-trip proptest (Task 11 of the plan) that generates random `InviteLinkBody` instances and asserts signature stability.
- **Base32 alphabet confusion.** RFC 4648 base32 has lowercase + uppercase variants; Tor onions use lowercase. Pin `base32::Alphabet::RFC4648 { padding: false }` with explicit lowercase output. `from_url` accepts both cases (lowercase via a `to_ascii_lowercase` normalize) to be forgiving of user copy/paste.
- **Base64url padding.** `base64::engine::general_purpose::URL_SAFE_NO_PAD`. Tests include "user pasted a padded URL" — we trim `=` padding on parse to be tolerant.
- **`InvitePsk` zeroize gap.** `InviteLinkBody.psk: [u8; 32]` is a plain array for serde. `from_url` must explicitly zero `body.psk` after copying to the guard. Test 12 above enforces this.
- **Migration 0003 idempotency.** The `UPDATE schema_version` clause only fires when current version = 2 (coming from 1.C's 0002). The migrations runner (Phase 0.D) applies unversioned migrations in order and tracks them via `schema_version`; adding 0003 to its `ALL_MIGRATIONS` array is the only wiring needed. Task 2 of the plan also extends the expected-tables assertion to include `contact_cards`.
- **`put_card` race.** The version-comparison-then-insert sequence must run inside a transaction to be safe against concurrent writers. Mitigation: `Pool::transaction(|tx| ...)` wrapping both the `SELECT max(version)` and the `INSERT`. `Pool::transaction` is already Phase 0.D's standard pattern.
- **ContactCard identity check.** `verify` returns the signer's pubkey; the caller is responsible for checking it against a known contact. If the caller forgets this check, a valid card for attacker A could get stored as a card for contact B. Tests enforce that storage's `put_card` matches the body's identity field against an existing contact row.

## Exit criteria

1. All unit tests in `invite::link`, `invite::qr`, `contact::card`, `storage::contacts` pass.
2. The integration test in `crates/tests/src/invite_roundtrip.rs` passes under `--features test-harness`.
3. `cargo fmt --check` / `cargo clippy --workspace --all-targets --all-features -- -D warnings` / `cargo test --workspace --all-features --release` all green.
4. `InviteLink::generate → to_url → from_url` round-trips; signatures verify; tampered signatures are rejected — verified by tests 1, 3.
5. Expired invite is rejected with `"invite: expired"` — test 4.
6. Single-use enforcement works: `is_consumed` returns true after `mark_consumed`, and a second `mark_consumed` is idempotent — tests 9, 10, 11.
7. `ContactCard::sign → verify` round-trips; stale version rejected at storage — tests 14, 19.
8. Migration 0003 applies cleanly on a fresh database and on a database previously at schema_version=2; `contact_cards` is in the expected-tables assertion.
9. CHANGELOG + CLAUDE.md refreshed with "Phase 1.D complete."
10. No PNG QR rendering, no onion rotation, no CLI wiring, no mailbox-list semantics (all explicitly out of scope).
