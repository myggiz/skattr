# Phase 2.E — Invite & contact UX design

**Status:** drafted 2026-05-03; awaiting plan.
**Predecessor:** Phase 2.D conversation view (merged 2026-05-02).
**Umbrella:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` § "2.E — Invite & contact UX".
**Kickoff:** `docs/superpowers/kickoffs/2026-05-02-phase-2e-invite-contact-ux-kickoff.md`.

## Scope

Phase 2.E delivers the invite & contact UX necessary for two
non-technical testers to complete the install → invite → first-message
loop end-to-end via the UI alone:

- Invite generate dialog (optional nickname, TTL preset, inline-rendered QR, copy-to-clipboard).
- Add-contact dialog (paste tab for `skattr://invite/v1#…`, scan tab via `getUserMedia` + `jsqr`).
- Contact details panel (pubkey short-hash with click-to-copy, current onion, mailbox list, inline rename, archive with confirm).
- **Task 2.E.0:** daemon-side fix to propagate the Welcome message from Bob (consumer of invite) to Alice (inviter), so Alice's MLS group transitions from `PendingJoin` to `Active` and she can decrypt Bob's outgoing messages.

Out of scope (deferred to 2.F or later): settings panel, mailbox CRUD UI, notifications, tray, packaging, multi-member groups, avatars/reactions/replies/edits, Welcome resend affordance, mailbox fallback for Welcome (tracked as **Task 2.E.5**).

**Exit criterion:** two non-technical testers complete the full
invite-from-Alice → add-by-Bob → first-message-from-Alice loop using
only the UI (no CLI). The Rust integration test
`crates/tests/src/welcome_propagation.rs` (`#[ignore]`-gated, real-Tor)
proves the wire-level Welcome propagation end-to-end.

## Locked decisions (from brainstorm 2026-05-03)

| # | Decision | Resolution |
|---|---|---|
| 1 | Welcome propagation transport | **Direct-only over `Frame::MlsWelcome`** (codec slot `0x03` already reserved). Mailbox fallback deferred to **Task 2.E.5**. |
| 2 | Inviter-side PSK persistence | New `outstanding_invites` table (migration `0010`); PSK zeroized on consume + on row delete. |
| 3 | `RenameContact` semantics | **Local-only.** `contacts.display_name` only; no `ContactCard` propagation. |
| 4 | `RemoveContact` semantics | **Soft-delete via `contacts.hidden` boolean** (migration `0011`). MLS state preserved. Idempotent. |
| 5 | Archive UX copy | "Archive [nickname]?" body: "[Nickname] disappears from your contacts. Messages stay encrypted on disk; you can unarchive from Settings → Archived." Buttons: `Archive` / `Cancel`. |
| 6 | `ListContacts` filter discriminator | **New `Command::ListContactsWithFilter { include_hidden: bool }` variant.** Existing `Command::ListContacts` unit variant is preserved (implicit `include_hidden = false`). Strictly additive. |
| 7 | Rename / archive event surface | **Reuse `Event::ContactUpdated(PublicKey)`.** UI's existing handler already re-fetches summaries. No new event. |
| 8 | TTL preset values | **Discrete radios** 1h / 6h / 24h / 7d, default 24h. No "never expires" sentinel. |
| 9 | QR rendering library | Existing `core::invite::qr::render_svg` (qrcode crate, MIT, feature `qr`). Wired via new `render_invite_qr` Tauri command. |
| 10 | QR scanning library | `jsqr` (MIT, ≈50 KiB). Synchronous decode of frames captured into a `<canvas>`. |
| 11 | Webcam permission flow | `getUserMedia` requested only when the Scan tab is opened. Stream stopped on tab switch / dialog close / unmount. On deny: "Camera access denied — paste an invite URL instead" with a button that switches to the Paste tab. |
| 12 | Contact details panel layout | **Inline expansion** under the contact row in the rail. No drawer, no modal. |
| 13 | Pubkey short-hash format | First 4 + last 4 hex with horizontal ellipsis: `7aa2c4d1…b3e9f701`. Click-to-copy copies the full 64-char hex; 1.5 s "Copied" toast. |
| 14 | Welcome ACK correlation | **Synthetic message id = `BLAKE2s(welcome_bytes)`** so the existing `Frame::Ack(MessageId)` correlator works unchanged. |

## §1 — Wire format (additive only)

Every change preserves CBOR backward decode: existing variants are not
reshaped; new fields default; new variants are added alongside the
existing ones.

### `Command` (3 new variants)

```rust
pub enum Command {
    // existing variants unchanged…

    /// Set or clear the local nickname for `contact`. Local-only —
    /// does not propagate to the peer.
    RenameContact {
        /// Peer identity pubkey.
        contact: PublicKey,
        /// `Some(nick)` sets; `None` clears. Empty / whitespace-only
        /// after trim is rejected as `InvalidArgument`.
        nickname: Option<String>,
    },

    /// Soft-delete a contact: flips `contacts.hidden = 1`. MLS state,
    /// messages, outbox, mailbox records are all preserved. Idempotent
    /// (re-archiving a hidden contact returns `Ok`).
    RemoveContact {
        /// Peer identity pubkey.
        contact: PublicKey,
    },

    /// Like `ListContacts` but with explicit `include_hidden` opt-in.
    /// `ListContacts` (the existing unit variant) implicitly passes
    /// `include_hidden = false` and behaves unchanged.
    ListContactsWithFilter {
        /// If `true`, include hidden (archived) contacts in the result.
        include_hidden: bool,
    },
}
```

### `CommandResult` — no new variants

`RenameContact` / `RemoveContact` return existing `CommandResult::Ok`.
`ListContactsWithFilter` returns existing `CommandResult::Contacts(Vec<ContactSummary>)`.

### `Event` — no new variants

Both rename and archive emit `Event::ContactUpdated(PublicKey)` (already
defined). The UI's existing handler re-fetches the summary, which
naturally reflects the new nickname or the contact's disappearance from
the default-filtered list.

### `Frame` — `MlsWelcome` (already defined, becomes load-bearing)

`crates/core/src/transport/frame.rs:40` reserves byte `0x03` for
`Frame::MlsWelcome(Vec<u8>)`; it is currently never sent or routed.
2.E activates this slot — no codec changes; the codec already
encodes / decodes / proptests the variant.

### `wire_format_append_only.rs` updates

Three new tags added to the `Command` snapshot: `rename_contact`,
`remove_contact`, `list_contacts_with_filter`. No additions to
`CommandResult`. Same alphabetical-sort + exhaustive-match discipline.

## §2 — Task 2.E.0: Welcome propagation

### §2.1 — Inviter-side PSK persistence

**Problem.** `create_invite` (`crates/core/src/daemon/dispatch.rs:172`)
generates a 32-byte PSK with `OsRng.fill_bytes`, embeds it in the URL
fragment via `InviteLink::generate`, and drops the local copy. MLS
`Group::join_from_welcome` requires the same PSK on both sides
(`crates/core/src/mls/group.rs:154`). When Alice receives the Welcome
emitted by Bob's `add_contact`, she has no way to recover the PSK
without persisting it at invite-create time.

**Solution.** Migration `0010` adds an `outstanding_invites` table:

```sql
-- 0010_outstanding_invites.sql
CREATE TABLE IF NOT EXISTS outstanding_invites (
    kp_hash      BLOB PRIMARY KEY,
    psk          BLOB NOT NULL,           -- 32 bytes; zeroized on consume
    inviter_kp   BLOB NOT NULL,           -- copy of our own KP for traceability
    expires_at   INTEGER NOT NULL,        -- unix seconds
    created_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_outstanding_invites_expires
    ON outstanding_invites(expires_at);
```

New repo `crates/core/src/storage/outstanding_invites.rs`:

```rust
pub(crate) struct OutstandingInviteRepo<'p> { pool: &'p Pool }

impl OutstandingInviteRepo<'_> {
    pub fn put(&self, kp_hash: &[u8; 32], psk: &Zeroizing<[u8; 32]>,
               inviter_kp: &[u8], expires_at: i64, created_at: i64) -> Result<()>;

    pub fn get_psk(&self, kp_hash: &[u8; 32]) -> Result<Option<(Zeroizing<[u8; 32]>, i64)>>;

    /// Zeroize the PSK column then delete the row. Implemented as
    /// `UPDATE … SET psk = zeroblob(32); DELETE FROM …` in a single tx.
    pub fn mark_consumed(&self, kp_hash: &[u8; 32]) -> Result<()>;

    pub fn purge_expired(&self, now: i64) -> Result<u64>;
}
```

`create_invite` writes the row immediately after `InviteLink::generate`:

```rust
let psk_arr = Zeroizing::new(psk);
oi_repo.put(&kp_hash, &psk_arr, &kp_bytes, now + ttl as i64, now)?;
```

The PSK is read back into `Zeroizing<[u8; 32]>` on Welcome receipt; the
buffer drops automatically at scope end.

### §2.2 — Bob-side: emit Welcome after AddContact

`add_contact` (`crates/core/src/daemon/dispatch.rs:222`) currently
discards the Welcome with `let (_welcome, _commit) = group.add_member(...)`.
Replace with:

```rust
let (welcome, _commit) =
    group.add_member(&invitee_kp, Some(&link.psk.0)).map_err(map_err)?;
let group_id = group.id().0.clone();

// existing: group.save, contact upsert, set_group_id, kp mark_consumed
// (already in place)

handle.hub.send_welcome(link.body.identity, welcome).await
    .map_err(map_err)?;

// Existing ContactAdded reply unchanged.
```

`DeliveryHub::send_welcome` is parallel to `send`:

```rust
pub async fn send_welcome(
    &self,
    peer: PublicKey,
    welcome_bytes: Vec<u8>,
) -> Result<oneshot::Receiver<std::result::Result<(), ()>>>;
```

Internally it calls `ensure_actor(peer)` (same lookup as `send`) and
submits a new `WelcomeJob { welcome_bytes, ack_tx }` over a new
`welcome_jobs: mpsc::Sender<WelcomeJob>` channel on `PeerChannels`. The
job's pending-ACK key is `MessageId(BLAKE2s(welcome_bytes)[..16])` — the
existing `pending: HashMap<MessageId, oneshot::Sender<…>>` is reused
unchanged.

### §2.3 — Peer actor: send + receive Welcome

`crates/core/src/delivery/peer.rs::full_run` gains:

**Send-side `select!` arm:**

```rust
job = welcome_jobs.recv() => {
    let Some(j) = job else { break };
    if let Some(c) = conn.as_mut() {
        let synthetic_id = welcome_msg_id(&j.welcome_bytes);  // BLAKE2s prefix
        if c.send(Frame::MlsWelcome(j.welcome_bytes)).await.is_err() {
            let _ = j.ack_tx.send(Err(()));
            conn = None;
            drain_pending(&mut pending);
        } else {
            pending.insert(synthetic_id, j.ack_tx);
        }
    } else {
        // No live conn — drop ack_tx (caller sees Err → outbox-style retry)
        let _ = j.ack_tx.send(Err(()));
    }
}
```

**Read-side `Frame::MlsWelcome` case:**

```rust
Ok(Some(Frame::MlsWelcome(welcome_bytes))) => {
    last_traffic = tokio::time::Instant::now();
    if let Some(d) = inbound.as_ref() {
        if let Some(synthetic_id) = d.dispatch_welcome(peer, &welcome_bytes) {
            if let Some(c) = conn.as_mut() {
                let _ = c.send(Frame::Ack(synthetic_id.0)).await;
            }
        }
        // None => rejected; do not ACK; sender retries once then gives up.
    } else {
        tracing::warn!(
            "peer: inbound MlsWelcome received but no InboundDispatch configured"
        );
    }
}
```

`welcome_msg_id` is a small helper:

```rust
fn welcome_msg_id(bytes: &[u8]) -> MessageId {
    use blake2::{Blake2s256, Digest};
    let mut h = Blake2s256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&out[..16]);
    MessageId(id)
}
```

Used identically on both sides so the ACK round-trips deterministically.

### §2.4 — `InboundDispatch::dispatch_welcome`

`crates/core/src/delivery/peer.rs` extends the trait:

```rust
pub trait InboundDispatch: Send + Sync + 'static {
    fn dispatch(&self, peer: PublicKey, ciphertext: &[u8]) -> Option<MessageId>;

    /// Process an inbound Welcome. Returns the synthetic
    /// `MessageId(BLAKE2s(welcome)[..16])` on success — caller ACKs
    /// with this — or `None` if the Welcome cannot be matched to an
    /// outstanding invite (unknown KP hash, expired, replay).
    fn dispatch_welcome(&self, peer: PublicKey, welcome: &[u8]) -> Option<MessageId>;
}
```

`DaemonInbound::dispatch_welcome` (in `crates/core/src/daemon/inbound.rs`):

1. Compute `synthetic_id = welcome_msg_id(welcome)`.
2. Parse `MlsMessageIn::tls_deserialize_exact(welcome)` and extract the
   target KeyPackage hash (`welcome_inner.secrets()[0].new_member()`-equivalent;
   exact API depends on OpenMLS 0.8 internals — fall back to walking the
   `secrets` slice if the public accessor is missing). On parse failure:
   `tracing::warn!`, return `None`.
3. `OutstandingInviteRepo::get_psk(&kp_hash)?` → `(psk, expires_at)`.
   - `None` (unknown hash): warn + return `None`. Treat the synthetic id as
     consumed for *that* call (peer retries are idempotent at the actor level).
   - `expires_at < now`: warn + return `None`; let `purge_expired` reap the row.
4. `Group::join_from_welcome(&handle.identity, welcome, Some(&psk.0), MlsProvider::new())?`.
5. In one `pool.transaction`:
   - `group.save_in_tx(&group_repo, tx)`
   - `contact_repo.upsert(&Contact { identity: peer, display_name: None, added_at: now, card: None })`
     (peer's pubkey is provided by the actor's transport-authenticated context; multi-member groups
     would parse it from the Welcome — out of scope for 2-member-only Phase 2)
   - `contact_repo.set_group_id(&peer, &group.id().0)`
   - `kp_repo.mark_consumed(&kp_hash)`
   - `oi_repo.mark_consumed(&kp_hash)` (zeroizes + deletes)
6. Emit `Event::ContactUpdated(peer)`.
7. Return `Some(synthetic_id)`.

Failure inside the transaction rolls back all five mutations; the actor
does not ACK; Bob retries once via the outbox-style oneshot.

**Peer pubkey resolution.** The actor calls
`dispatch_welcome(peer, &bytes)` where `peer` is the
Noise_XK-authenticated static key — i.e. Bob's identity pubkey, learned
during the handshake. This is exactly what we want to upsert as Alice's
local contact for Bob.

### §2.5 — Sweeping expired invites

`daemon::retention` (the existing hourly tick from 1.G) gains a single
extra step:

```rust
let oi_repo = OutstandingInviteRepo::new(&pool);
match oi_repo.purge_expired(now_unix_seconds()) {
    Ok(n) if n > 0 => tracing::debug!(rows = n, "retention: purged expired invites"),
    Err(e) => tracing::warn!(err = %e, "retention: purge_expired failed"),
    _ => {}
}
```

PSK rows are zeroized via `UPDATE psk = zeroblob(32)` before the
`DELETE` so the SQLite page that held the old bytes is overwritten in
place.

### §2.6 — Mailbox fallback for Welcome — deferred (Task 2.E.5)

Direct-only is sufficient for 2.E's exit criterion: the typical
add-contact happens within minutes of Alice generating the invite,
while she's still online. If Alice is offline, Bob's send_welcome
oneshot resolves `Err`, the existing `DeliveryStatusChanged(Failed)`
event surfaces, and the recovery is "Alice regenerates the invite"
(single-use semantics make this safe).

Mailbox fallback for Welcome bytes is non-trivial because the existing
`InboundDispatch::dispatch(peer, ciphertext)` path used by
`mailbox/poll.rs:450` has no payload-kind discriminator. Adding one
touches the 2.B mailbox protocol freeze (ADR 0006) and warrants its own
ADR. Out of scope here; tracked alongside Tasks 20.5 / 22.5 / 23.5 in
CLAUDE.md as **Task 2.E.5**.

## §3 — Daemon dispatch

### §3.1 — `RenameContact`

```rust
async fn rename_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: PublicKey,
    nickname: Option<String>,
) -> std::result::Result<CommandResult, IpcError> {
    let trimmed = match nickname {
        None => None,
        Some(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument));
            }
            if t.chars().count() > 64 {
                return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument));
            }
            Some(t)
        }
    };

    let repo = ContactRepo::new(&handle.pool);
    repo.set_display_name(&contact, trimmed.as_deref()).map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}
```

`ContactRepo::set_display_name(identity, name: Option<&str>) -> Result<()>`
is a single `UPDATE contacts SET display_name = ?1 WHERE identity_pubkey = ?2`.
Returns `CoreError::Contact(ContactErrorKind::NotFound)` if no row was
changed.

### §3.2 — `RemoveContact`

```rust
async fn remove_contact<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: PublicKey,
) -> std::result::Result<CommandResult, IpcError> {
    let repo = ContactRepo::new(&handle.pool);
    repo.set_hidden(&contact, true).map_err(map_err)?;
    let _ = handle.events_tx.send(Event::ContactUpdated(contact));
    Ok(CommandResult::Ok)
}
```

`ContactRepo::set_hidden(identity, hidden: bool) -> Result<()>` —
single `UPDATE`. Idempotent: re-archiving a hidden contact succeeds
silently. `NotFound` returned if no row matched.

MLS group state, `contacts.group_id`, messages, outbox, mailboxes, and
`read_state` are all untouched.

### §3.3 — `ListContactsWithFilter`

The existing `list_contacts` handler is refactored to take an
`include_hidden: bool` parameter. The unit `Command::ListContacts`
calls it with `false`; `Command::ListContactsWithFilter { include_hidden }`
forwards the parameter. The query becomes:

```rust
let contacts = if include_hidden {
    repo.list_all()?
} else {
    repo.list()?      // existing — only WHERE hidden = 0
};
```

`ContactRepo::list_all()` is a new sibling of `list()` that omits the
`hidden = 0` predicate. The existing `list()` is updated to filter
`WHERE hidden = 0` once migration `0011` is applied. Sort order is
unchanged.

### §3.4 — Migration 0011: contacts.hidden

```sql
-- 0011_contacts_hidden.sql
ALTER TABLE contacts ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_contacts_hidden ON contacts(hidden);
```

`INTEGER` (SQLite has no native `BOOLEAN`) treated as `0|1`. The
`DEFAULT 0` ensures all existing rows remain visible.

## §4 — UI components

### Component tree (new + modified)

```
crates/ui/src-svelte/src/lib/components/
├── InviteGenerateDialog.svelte    [NEW]
├── AddContactDialog.svelte        [NEW]
├── ContactDetailsPanel.svelte     [NEW]
├── ConfirmDialog.svelte           [NEW]
├── Toast.svelte                   [NEW]
├── ContactRow.svelte              [MODIFIED: chevron + expand]
└── (existing components unchanged)
```

### `InviteGenerateDialog.svelte`

Modal dialog. Two-step state machine:

**Step 1 (form):**
- Optional nickname textbox (≤ 64 chars).
- TTL preset radios: 1h / 6h / 24h / 7d (default `24h` checked).
- Buttons: `Generate` / `Cancel`.

**Step 2 (result):**
- URL in a monospaced `<code>` block; click-to-copy with 1.5 s "Copied" toast.
- Inline SVG QR (rendered via `render_invite_qr` Tauri command).
- Expiry countdown ("Expires in 23h 47m") — refreshed every 30 s.
- Buttons: `Copy URL` / `Done`.

On Generate: calls `Command::CreateInvite { nickname, ttl_secs }` →
receives `CommandResult::InviteCreated { url, key_package_id, expires_at }` →
calls Tauri `render_invite_qr(url)` → renders SVG. Errors surfaced inline
(daemon-down, Tor-not-ready, etc.).

### `AddContactDialog.svelte`

Modal dialog with two tabs (`Paste` / `Scan`). Tab state is local; the
camera stream is bound to "Scan tab is active."

**Paste tab:**
- `<textarea>` for the URL (placeholder: `skattr://invite/v1#…`).
- `Add contact` button → `Command::AddContact { invite_url }`.
- Errors surfaced inline below the textarea.

**Scan tab:**
- On tab activation: `navigator.mediaDevices.getUserMedia({ video: true })`.
- Render frames into a `<canvas>` via `requestAnimationFrame`.
- Per frame: `jsqr(data, w, h, { inversionAttempts: "dontInvert" })`.
- On match: pause stream, show URL preview + `Add` / `Try again` buttons.
- On `getUserMedia` reject: show "Camera access denied — paste an invite URL instead" with a `Switch to Paste tab` button.
- On dialog close / tab switch / unmount: stream tracks stopped via `stream.getTracks().forEach(t => t.stop())`.

`jsqr` is bundled via `pnpm add jsqr` (MIT, ≈ 50 KiB minified). No
remote import; bundled by Vite.

### `ContactDetailsPanel.svelte`

Inline-expanded row beneath the contact in the rail. Toggles via a
chevron button on `ContactRow.svelte`; expanded state lives in the
`contacts` store (single-select — opening one closes others).

Sections (top to bottom):

**Identity**
- Pubkey: `7aa2c4d1…b3e9f701` (computed: `pubkey.slice(0, 8) + "…" + pubkey.slice(-8)`).
  Click-to-copy copies the full 64-char hex; 1.5 s "Copied" toast.
- Onion: `7aa2…f701.onion` truncated similarly; click-to-copy full.

**Mailboxes** (placeholder for 2.E)
- Heading "Peer mailboxes".
- For 2.E: shows "No mailboxes" if `ContactSummary` doesn't carry peer
  mailboxes (it doesn't yet — `ContactCard.body.mailboxes` is not
  projected onto `ContactSummary`; that projection lands in 2.F when
  the settings panel needs it).
- The data model is forward-compatible: 2.F adds
  `ContactSummary.peer_mailboxes: Vec<String>` (additive,
  `#[serde(default)]`) and this section renders the list.

**Rename**
- Inline `<input>` pre-filled with current `nickname`.
- Buttons: `Save` (calls `Command::RenameContact`) / `Cancel` (resets).
- Validation: empty after trim or > 64 chars → inline error, button disabled.

**Archive**
- Red "Archive" button → opens `ConfirmDialog` with locked copy.
- On confirm: calls `Command::RemoveContact`. On success: panel collapses,
  `Event::ContactUpdated` fires, the row disappears from the list.

### `ConfirmDialog.svelte`

Reusable modal with locked props:
```ts
{
  title: string;
  body: string;
  confirmLabel: string;
  cancelLabel?: string;       // default "Cancel"
  danger?: boolean;            // styles confirm button with --danger
  onConfirm: () => Promise<void>;
}
```
Used by Archive in 2.E; future re-uses in 2.F (passphrase change confirm, etc.).

### `Toast.svelte`

Non-modal transient notification anchored bottom-right. Auto-dismisses
after 1.5 s. Single live toast at a time (new toast replaces previous).
Used for "Copied", "Camera access denied", "Invite added", etc.

### Stores

**`contacts.ts` (extended):**

```ts
interface ContactsState {
  list: ContactSummary[];
  expandedPubkey: string | null;          // NEW: which row is detail-expanded
}

export async function rename(contact: string, nickname: string | null);
export async function archive(contact: string);
export function toggleExpanded(pubkey: string);   // single-select
```

`rename` / `archive` await IPC then call `refreshContacts()`. The store
subscribes to `Event::ContactUpdated` (via the existing event handler
in `+page.svelte`) and refreshes there.

**`qr.ts` (new):**

```ts
const cache = new Map<string, string>();   // url → svg

export async function renderInviteQr(url: string): Promise<string> {
  if (cache.has(url)) return cache.get(url)!;
  const svg = await invoke<string>("render_invite_qr", { url });
  cache.set(url, svg);
  return svg;
}
```

### Routes

`routes/+page.svelte` gains:

- A "+" button in the rail header that opens `AddContactDialog`.
- A "Generate invite" button next to the rail header (or in an action menu).
- Wires `ContactRow` chevron click to `toggleExpanded(pubkey)`.
- Renders `ContactDetailsPanel` when `$contacts.expandedPubkey === c.pubkey`.

No new route paths.

### Tauri command

New post-daemon Tauri command in `crates/ui/src/ipc_bridge.rs`:

```rust
#[tauri::command]
pub async fn render_invite_qr(url: String) -> Result<String, String> {
    use skattr_core::invite::InviteLink;
    let link = InviteLink::from_url(&url, now_unix_seconds())
        .map_err(|e| format!("parse invite: {e}"))?;
    skattr_core::invite::qr::render_svg(&link)
        .map_err(|e| format!("render qr: {e}"))
}
```

`crates/ui/Cargo.toml` enables the `qr` feature on its
`skattr-core` dep so the `qr` module compiles in.

The command handler is registered in `crates/ui/src/main.rs` alongside
the existing `bootstrap::*` and `ipc_bridge::ipc_request` handlers — its
own `invoke_handler` entry, no `tauri.conf.json` ACL change needed
(post-daemon allowlist already permits arbitrary handlers).

## §5 — Error handling and edge cases

| Case | Behaviour |
|---|---|
| Welcome arrives for a KP whose `outstanding_invites` row is missing | `tracing::warn!`, return `None` (no ACK); sender's actor retries once then gives up |
| Welcome arrives for an expired invite | Same as above; row sweeped by retention tick |
| Welcome parse fails (corrupt bytes) | Same as above |
| `Group::join_from_welcome` fails (PSK mismatch, MLS internal error) | Same; the outstanding_invites row stays — operator can investigate |
| `RenameContact` with empty / whitespace-only nickname | `IpcError::Daemon(InvalidArgument)`; UI shows inline validation, button disabled until valid |
| `RenameContact` with > 64-char nickname | Same as above |
| `RenameContact` for non-existent contact | `IpcError::Daemon(...)` carrying `ContactErrorKind::NotFound` |
| `RemoveContact` on already-hidden contact | `Ok` (idempotent) |
| `RemoveContact` on non-existent contact | `IpcError::Daemon(...)` carrying `ContactErrorKind::NotFound` |
| `AddContact` with malformed URL | Existing `InviteErrorKind::Other` path, surfaced inline in dialog |
| QR scan camera permission denied | Tab message + "Switch to Paste tab" button |
| QR scan returns a non-`skattr://invite/v1#` URL | Toast: "This doesn't look like a skattr invite"; stream keeps running |
| QR scan returns a valid URL twice in a row | Same URL → no-op (debounced via "show preview, await user confirm") |
| Welcome sent but ACK lost mid-flight | Bob's send_welcome oneshot resolves `Err`; UI shows "Couldn't reach Alice — she may need to regenerate the invite". The KP is already consumed — Alice cannot reissue the *same* KP, but `CreateInvite` always generates a fresh one |
| Bob's `add_contact` succeeds but `send_welcome` fails | `Command::AddContact` reply is still `ContactAdded` (the local rows are persisted). The contact appears in Bob's rail with `group_state: Active`. Bob can send messages to Alice immediately, but Alice can't decrypt them until she gets a Welcome. UI surfaces this via the existing `delivery_status_changed: Failed` flow plus a one-time toast: "Couldn't deliver your invitation to [nickname]." Recovery: regenerate invite from Alice. |
| Daemon down at dialog open | Standard "Daemon not running" disabled state from 2.C |

The **Bob's add_contact succeeds, send_welcome fails** branch is the
only meaningful new failure mode in 2.E. Phase 3+ can introduce a
"resend Welcome" affordance once mailbox fallback (Task 2.E.5) lands.

## §6 — Testing strategy

### §6.1 — Rust unit + integration

| Test | Asserts |
|---|---|
| `storage::outstanding_invites::tests::put_get_roundtrip` | Round-trips PSK + expires_at |
| `storage::outstanding_invites::tests::mark_consumed_zeroizes_then_deletes` | After mark_consumed, get_psk returns None; the freed page contains zeros (best-effort assertion) |
| `storage::outstanding_invites::tests::purge_expired_removes_only_expired` | Two rows, one expired; purge_expired returns 1; non-expired remains |
| `storage::contacts::tests::set_display_name_round_trips` | Set → get matches |
| `storage::contacts::tests::set_display_name_clears_on_none` | Set Some → set None → get returns None |
| `storage::contacts::tests::set_display_name_not_found` | Returns ContactErrorKind::NotFound |
| `storage::contacts::tests::set_hidden_round_trips` | Set true → list() omits → list_all() includes |
| `storage::contacts::tests::set_hidden_idempotent` | Two consecutive set_hidden(true) succeed |
| `daemon::dispatch::tests::create_invite_persists_outstanding_invite` | After CreateInvite, outstanding_invites has the row with matching kp_hash |
| `daemon::dispatch::tests::add_contact_emits_welcome_to_hub` | Mock hub asserts send_welcome called with correct peer + non-empty bytes |
| `daemon::dispatch::tests::rename_contact_validates_nickname` | Three cases: empty after trim, > 64 chars, valid happy path |
| `daemon::dispatch::tests::rename_contact_emits_contact_updated_event` | Event subscriber receives ContactUpdated(peer) |
| `daemon::dispatch::tests::remove_contact_is_idempotent` | Two consecutive RemoveContact succeed; row is still hidden |
| `daemon::dispatch::tests::remove_contact_preserves_mls_group_state` | After RemoveContact, MlsGroupRepo::get returns Some unchanged blob |
| `daemon::dispatch::tests::list_contacts_filters_hidden_by_default` | Two contacts, one hidden; ListContacts returns only visible |
| `daemon::dispatch::tests::list_contacts_with_filter_includes_hidden` | Same fixture; ListContactsWithFilter { include_hidden: true } returns both |
| `daemon::inbound::tests::dispatch_welcome_joins_group_and_emits_contact_updated` | Outstanding invite present; Welcome processed; group is Active; ContactUpdated emitted |
| `daemon::inbound::tests::dispatch_welcome_rejects_unknown_kp_hash` | Returns None; no rows mutated; no event |
| `daemon::inbound::tests::dispatch_welcome_rejects_expired_invite` | Returns None; row exists but expired; no event |
| `daemon::inbound::tests::dispatch_welcome_idempotent_on_replay` | Second dispatch_welcome with the same bytes returns None (kp_hash already consumed); no double-Active transitions |
| **NEW** `crates/tests/src/welcome_propagation.rs` (`#[ignore]`-gated, real-Tor) | Two daemons paired; Alice CreateInvite; Bob AddContact; Alice's group transitions PendingJoin → Active within 5 s; round-trip SendMessage Alice→Bob and Bob→Alice both decrypt |
| Existing `cli_two_daemons` extended | Assert Alice's `ContactSummary.group_state` is `Active` after Bob's AddContact + Welcome propagation |

### §6.2 — TypeScript unit (Vitest)

| Test | Asserts |
|---|---|
| `InviteGenerateDialog.test.ts: ttl_radios_send_correct_seconds` | 1h → 3600, 6h → 21600, 24h → 86400 (default), 7d → 604800 |
| `InviteGenerateDialog.test.ts: copy_writes_full_url_to_clipboard` | Mock `navigator.clipboard.writeText`; assert called with full URL |
| `InviteGenerateDialog.test.ts: qr_command_called_with_url` | Mock `invoke`; assert `render_invite_qr` called once with the URL |
| `InviteGenerateDialog.test.ts: qr_cached_on_second_render` | Two reads of the same URL → one invoke call |
| `AddContactDialog.test.ts: paste_submits_invite` | `Command::AddContact { invite_url }` dispatched on click |
| `AddContactDialog.test.ts: scan_tab_requests_camera` | Mock `navigator.mediaDevices.getUserMedia`; assert called on tab open |
| `AddContactDialog.test.ts: scan_tab_deny_shows_fallback` | Mock reject; "Camera access denied" + Switch-to-Paste button visible |
| `AddContactDialog.test.ts: scan_tab_close_stops_stream` | Mock `MediaStream.getTracks().forEach(t => t.stop())`; assert called on dialog close |
| `ContactDetailsPanel.test.ts: short_hash_format` | Regex `^[0-9a-f]{8}…[0-9a-f]{8}$` on rendered span |
| `ContactDetailsPanel.test.ts: click_pubkey_copies_full_hex` | Mock clipboard; assert called with full 64-char hex; toast renders |
| `ContactDetailsPanel.test.ts: rename_disabled_until_valid` | Empty / whitespace / > 64 → button disabled; valid → enabled |
| `ContactDetailsPanel.test.ts: rename_submit_calls_ipc` | Click Save → `Command::RenameContact { contact, nickname }` |
| `ContactDetailsPanel.test.ts: archive_opens_confirm_dialog` | Click Archive → ConfirmDialog with locked copy renders |
| `ContactDetailsPanel.test.ts: archive_confirm_calls_ipc` | Confirm → `Command::RemoveContact { contact }` |
| `contacts.store.test.ts: rename_refreshes_on_success` | After rename → `list_contacts` IPC fired again |
| `contacts.store.test.ts: archive_refreshes_on_success` | After archive → `list_contacts` IPC fired again |
| `contacts.store.test.ts: toggle_expanded_single_select` | Open A → expandedPubkey == A; open B → expandedPubkey == B (A closed) |

### §6.3 — Playwright e2e

| Spec | Flow |
|---|---|
| `invite-generate.spec.ts` | Open generate dialog → pick 24h → Generate → assert URL appears + QR `<svg>` renders + Copy button works (mocked clipboard) |
| `add-contact-paste.spec.ts` | Open add-contact dialog → paste fixture URL → Add → contact appears in rail with the fixture nickname → expand details → Archive → contact disappears |
| `contact-details-panel.spec.ts` | Open details → assert pubkey short-hash format → click rename → save new nickname → assert rail updates → Archive → assert contact gone |

Tauri-mock fixtures extended in `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`:
- `?fixture=invite-flow` — seeds vault, returns a deterministic CreateInvite reply, mocks `render_invite_qr` to return a stable inline SVG.
- `?fixture=add-contact-flow` — seeds vault + accepts AddContact with a deterministic peer pubkey + nickname.

Scan-tab Playwright coverage is **out of scope** (jsdom can't render
`getUserMedia`); a manual test plan is documented in the implementation
plan deliverable.

### §6.4 — Lint / spec compliance

| Test | Asserts |
|---|---|
| `crates/core/tests/wire_format_append_only.rs` | Updated lists carry `rename_contact`, `remove_contact`, `list_contacts_with_filter` |
| Existing `lint_no_remote_assets.test.ts` (from 2.C) | Continues to pass — `jsqr` is bundled, the QR SVG is daemon-generated, the qr-code icon is local |

## §7 — Risks and mitigations

| Risk | Mitigation |
|---|---|
| OpenMLS 0.8 may not expose KP hash from `Welcome` cleanly | Pre-fly check during implementation: `MlsMessageIn::tls_deserialize_exact` + walk `welcome_inner.secrets()` for `EncryptedGroupSecrets.new_member`. If only available via `pub(crate)`, add a thin shim in `mls/key_package.rs` that owns the parse. Test with hand-crafted Welcome bytes |
| Synthetic-id collision: two distinct Welcomes with the same BLAKE2s prefix | Negligible (2^-64 birthday for 16-byte prefix); we are already implicitly OK with this for `MessageId` (which is also 16 bytes). Document the reasoning |
| Bob's `add_contact` returns success before Welcome ACK | Locked behaviour; documented in §5. UI shows a `delivery_status_changed: Failed` toast if the oneshot resolves Err |
| `getUserMedia` permission persists across dialog opens (Chromium remembers grants) | Acceptable — user can revoke via browser settings. Document in onboarding copy |
| `jsqr` mis-decodes a fragment of an unrelated QR as a skattr URL | URL prefix check rejects non-`skattr://invite/v1#` matches. False positives are silently ignored — the user re-points the camera |
| Inline expansion crowds the rail when multiple panels are open | Single-select policy: opening one closes others (enforced in store) |
| Migration ordering: 0010 (outstanding_invites) and 0011 (contacts.hidden) added in same phase | Migrations runner is keyed by `schema_version` (existing); each migration runs once, ordered by filename. No special handling |
| `outstanding_invites.psk` blob recoverability after `mark_consumed` | We zeroize via `UPDATE psk = zeroblob(32)` BEFORE `DELETE` so the SQLite page that held the bytes is overwritten in place. Acceptable per CLAUDE.md "secrets zeroize" rule; not a forensic guarantee |
| `Frame::MlsWelcome` slot has been reserved but never sent — first usage may surface codec bugs | The codec is already proptest-covered (`crates/core/tests/frame_proptest.rs`) for arbitrary payloads; new integration test exercises the real send/recv path end-to-end |

## §8 — Out of scope (deferred)

- Mailbox fallback for Welcome propagation — **Task 2.E.5** follow-up.
- "Show archived" view + `Command::RestoreContact` — Phase 3+.
- Welcome resend affordance — Phase 3+ (depends on 2.E.5).
- Avatars / reactions / replies / edits / typing indicators — Phase 3.
- Attachments / file send — Phase 3.
- Multi-member groups — Phase 3.
- Settings panel / mailbox CRUD UI / notifications / tray — 2.F.
- Packaging / installers — 2.G.
- Phase 2.B follow-ups (Tasks 20.5 / 22.5 / 23.5) — independent, tracked in CLAUDE.md.
- `ContactCard.body.mailboxes` projection onto `ContactSummary.peer_mailboxes` — 2.F (when settings needs it).
- Wire-format BREAKING changes — anything renaming or removing a Command / CommandResult / Event variant requires a separate spec.
- Real HS key rotation — Task 23.5 (Phase 2.B follow-up).
- CLI surface for `RenameContact` / `RemoveContact` / `ListContactsWithFilter` — out of scope for 2.E (CLI is unaffected; future phase can add `skattr contact rename` / `archive` subcommands if desired).

## §9 — Files touched (preview, exhaustive in plan)

**Rust (`crates/core/`):**
- `daemon/commands.rs` — add `RenameContact`, `RemoveContact`, `ListContactsWithFilter` `Command` variants
- `daemon/dispatch.rs` — extend `add_contact` with `send_welcome`; add `rename_contact`, `remove_contact`, refactor `list_contacts` to take `include_hidden`
- `daemon/inbound.rs` — add `dispatch_welcome` impl
- `daemon/retention.rs` — add `purge_expired` step
- `delivery/hub.rs` — add `send_welcome`, `welcome_jobs` channel on `PeerChannels`
- `delivery/peer.rs` — add `WelcomeJob` send arm + `Frame::MlsWelcome` read arm; extend `InboundDispatch` trait with `dispatch_welcome`; helper `welcome_msg_id`
- `mls/key_package.rs` — possibly add a Welcome-parse shim if OpenMLS internals require it
- `storage/migrations/0010_outstanding_invites.sql` — NEW
- `storage/migrations/0011_contacts_hidden.sql` — NEW
- `storage/outstanding_invites.rs` — NEW repo
- `storage/contacts.rs` — `set_display_name`, `set_hidden`, `list_all`; update `list` to filter `hidden = 0`
- `storage/mod.rs` — export new repo
- `tests/wire_format_append_only.rs` — update Command snapshot

**Rust (`crates/cli/`):** unchanged.

**Rust (`crates/tests/`):**
- `src/welcome_propagation.rs` — NEW (`#[ignore]`-gated, real-Tor)
- `src/cli_two_daemons.rs` — assert Alice's group_state == Active after AddContact

**Rust (`crates/ui/src/`):**
- `ipc_bridge.rs` — register `render_invite_qr`
- `main.rs` — add `render_invite_qr` to `invoke_handler!`
- `Cargo.toml` — enable `qr` feature on `skattr-core`

**TypeScript (`crates/ui/src-svelte/src/`):**
- `lib/components/InviteGenerateDialog.svelte` — NEW
- `lib/components/AddContactDialog.svelte` — NEW
- `lib/components/ContactDetailsPanel.svelte` — NEW
- `lib/components/ConfirmDialog.svelte` — NEW
- `lib/components/Toast.svelte` — NEW
- `lib/components/ContactRow.svelte` — chevron + expand affordance
- `lib/icons/qr-code.svg` — NEW (Lucide MIT)
- `lib/stores/contacts.ts` — extend with `rename`, `archive`, `toggleExpanded`, `expandedPubkey`
- `lib/stores/qr.ts` — NEW
- `lib/test/tauri-mock.ts` — extend with `invite-flow` / `add-contact-flow` fixtures
- `routes/+page.svelte` — wire dialogs, expansion, header buttons
- `package.json` — add `jsqr` (MIT)

**Tests:**
- `crates/ui/src-svelte/src/lib/components/*.test.ts` — Vitest specs per §6.2
- `crates/ui/src-svelte/tests/e2e/invite-generate.spec.ts` — NEW
- `crates/ui/src-svelte/tests/e2e/add-contact-paste.spec.ts` — NEW
- `crates/ui/src-svelte/tests/e2e/contact-details-panel.spec.ts` — NEW
