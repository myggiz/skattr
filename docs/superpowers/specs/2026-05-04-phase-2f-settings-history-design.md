# Phase 2.F — Settings & History — Design

**Status:** draft, pending user review.
**Date:** 2026-05-04.
**Predecessor:** Phase 2.E (invite & contact UX) merged 2026-05-03 (`d4c9bc7`).
**Decomposition parent:** `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` § "2.F — Settings & history".
**Kickoff prompt:** `docs/superpowers/kickoffs/2026-05-03-phase-2f-settings-history-kickoff.md`.
**Brainstormed against:** Phase 2.B (mailbox client), Phase 2.E (invite & contact UX), Phase 1.G (history search), Phase 0.B (identity vault), Phase 0.D (storage age key).

## Scope

Phase 2.F closes Phase 2's user-facing chrome. Five sub-deliverables, all
landing in one merge to master:

1. **Settings transport + UI** — `Command::GetConfig` / `Command::SetConfig
   { patch: ConfigPatch }`, mailbox CRUD wired against 2.B's `MailboxClient`,
   sidebar-nav `routes/settings/` with five sections, contact-mute wire
   surface (`Command::SetContactMuted`, additive `ContactSummary.muted`).
2. **Security ops** — `Command::ChangePassphrase` with a journaled atomic
   re-key spanning identity vault + storage age key. `DaemonInbound::set_identity`
   refresh after re-key (defensive symmetry with future seed-rotating flows).
3. **Notifications** — `notify-rust` integration; focus-aware suppression;
   per-conversation mute persisted daemon-side; cross-OS smoke checklist.
4. **Tray + close-to-tray** — Tauri 2 built-in tray with Show / status-only /
   Quit menu; close button hides to tray by default; one-time toast.
5. **Cross-conversation search UX** — Cmd/Ctrl-K palette over 1.G's
   `Command::SearchMessages`; same component reused inline in Settings →
   History.

Wire-format additions are strictly additive. The `wire_format_append_only`
snapshot test enumeration is updated in the same commit as each new variant.

## Locked decisions (do not relitigate inside 2.F)

| ID  | Decision                                                                                                  |
|-----|-----------------------------------------------------------------------------------------------------------|
| 1   | Settings UI ships as nested SvelteKit routes under `routes/settings/<section>/+page.svelte` with a sidebar `+layout.svelte`, NOT a single scrollable pane and NOT a tab strip. Five sections: Identity, Mailboxes, History, Notifications, Advanced. |
| 2   | IPC shape is hybrid: `Command::GetConfig` + `Command::SetConfig { patch: ConfigPatch }` for normal config; security-sensitive operations (`ChangePassphrase`, `RotateOnion`, `WipeAllData`) stay as their own commands. |
| 3   | `ChangePassphrase` uses a **stage-then-rename** atomic re-key: both re-encrypted files are staged on disk, fsynced, the journal is written, then both renames happen in sequence. Recovery on boot probes the on-disk state, completes whatever rename is missing, and never needs the OLD passphrase from the user. |
| 4   | Notifications: four modes (`full`, `minimal`, `generic`, `off`); focus-aware suppression fires only when window is focused AND the active conversation is the recipient — other conversations always notify. The behaviour is hard-locked and not configurable. |
| 5   | Per-conversation mute is daemon-side via a new `contacts.muted` column (migration `0013`) and a new `Command::SetContactMuted`. Surfaces additively as `ContactSummary.muted: bool`. |
| 6   | Tray uses Tauri 2's built-in `tray::TrayIconBuilder`. Menu: Show window, Tor status (disabled), Unread count (disabled, hidden when 0), Quit. Click-icon toggles window visibility. |
| 7   | Close-to-tray is configurable via `ConfigPatch.close_to_tray` (default `true`); start-minimised via `start_minimised` (default `false`). Power users can disable both. |
| 8   | Cross-conversation search is a Cmd/Ctrl-K modal palette plus the same component mounted inline in Settings → History. Result-click navigates to the conversation at that row, briefly highlights it, but does NOT advance the read cursor. |
| 9   | Retention slider presets: `24h / 7d / 30d / 90d / Never`. (Drop the kickoff's `1y` as redundant with Never; add `24h` for paranoid users.) |
| 10  | History export: both JSONL and Plaintext, user picks per export; gzip optional (default off). Filename `skattr-export-<YYYYMMDD>.<ext>[.gz]` for "all", `skattr-<contact-shorthash>-<YYYYMMDD>.<ext>[.gz]` for per-conversation. |
| 11  | Logs viewer: in-memory ring buffer (≤ 5000 records) streamed via `Event::LogRecord` (filter-gated). Optional disk persistence via `ConfigPatch.persist_logs_to_disk` (default `false`); when toggled on, a `tracing-appender::rolling::daily` writer is hot-added through a `tracing-subscriber::reload::Layer`. Off-toggle does NOT delete already-persisted files (documented). |
| 12  | `peer_mailboxes` projection: additive `peer_mailboxes: Vec<String>` on `ContactSummary`, populated from the latest verified `ContactCard.body.mailboxes` for the contact. |
| 13  | "Wipe all data" is a real command (`Command::WipeAllData`) under Settings → Advanced → Danger zone. Confirms twice; daemon shuts down `Pool`, `remove_dir_all(data_dir)`, exits with code 0. |

## Architecture

Three roughly-independent slices, each landable in its own commit:

```
crates/core/
  src/
    daemon/
      config.rs            # ConfigPatch alongside Config; Config gains [delivery] (direct_timeout_secs)
                           # + [notifications] (mode) + [ui] (close_to_tray, start_minimised,
                           # persist_logs_to_disk) sections — all #[serde(default)] so old config.toml
                           # files keep parsing
      commands.rs          # +GetConfig, +SetConfig, +ChangePassphrase, +SetContactMuted, +TailLogs,
                           #  +GetPassphraseAuditLatest, +WipeAllData
                           # +Config(ConfigSnapshot), +PassphraseChanged, +Logs, +PassphraseAudit results
      events.rs            # +Event::LogRecord (filter-gated)
      ipc/wire.rs          # +EventFilter::Logs
      logs.rs              # NEW: ring-buffer tracing layer + redaction + IPC stream
      passphrase.rs        # NEW: re-key journal + recovery
      retention.rs         # unchanged (already honours [history] retention_days)
      dispatch.rs          # +SetConfig, +ChangePassphrase, +SetContactMuted, +TailLogs, +WipeAllData,
                           #  +GetPassphraseAuditLatest handlers; mailbox CRUD bodies replace stubs
    contact/repo.rs        # +ContactRepo::set_muted; +peer_mailboxes projection in summary query
    storage/migrations/
      0013_contacts_muted.sql
      0014_passphrase_audit.sql
crates/ui/
  src-svelte/src/
    routes/
      settings/
        +layout.svelte                 # sidebar nav; ESC returns to /
        identity/+page.svelte
        mailboxes/+page.svelte
        history/+page.svelte
        notifications/+page.svelte
        advanced/+page.svelte
      conversation/[contact]/+page.svelte    # extended: focus_row_id query param handling
    lib/
      components/
        SearchPalette.svelte           # Cmd/Ctrl-K modal; reused inline in Settings → History
        ChangePassphraseDialog.svelte
        ConfirmDialog.svelte           # already exists from 2.E; reused for prune + WipeAllData (no new component)
        ContactDetailsPanel.svelte     # +mute toggle; +peer_mailboxes render (extends 2.E component)
      stores/
        config.ts                      # mirror of GetConfig snapshot; debounced SetConfig writer
        focus.ts                       # tracks windowFocused + activeContactId
        logs.ts                        # subscribes to Event::LogRecord when Advanced/Logs is open
        searchPalette.ts               # palette open/query/results
      Notifications/
        dispatcher.ts                  # focus-aware notification dispatcher
        dispatcher.test.ts
  src/
    tray.rs                            # tauri::tray::TrayIconBuilder + click handlers
    notifications.rs                   # notify-rust wrapper (Tauri command)
    main.rs                            # +tray init, +close-to-hide handler, +start_minimised
crates/tests/src/
  passphrase_atomicity.rs              # six kill-points × two recovery outcomes
  settings_roundtrip.rs                # GetConfig → SetConfig → GetConfig
  mailbox_crud.rs                      # add → list → remove against real 2.B mailbox client
  wipe_data.rs                         # WipeAllData removes data_dir, exits 0
docs/operations/
  2f-notification-smoke.md             # per-OS manual checklist
  passphrase-recovery.md               # journal explanation + manual recovery procedure
```

Visibility: `daemon::passphrase` and `daemon::logs` are `pub(crate)`. Only their `Command` / `CommandResult` / `Event` variants leak through the existing public IPC surface.

## Wire-format contract (additive only)

### New `Command` variants

```rust
Command::GetConfig
Command::SetConfig { patch: ConfigPatch }
Command::ChangePassphrase { old: String, new: String }   // wrapped in Zeroizing<String> on the daemon as soon as decoded
Command::SetContactMuted { contact: PublicKey, muted: bool }
Command::TailLogs { since_seq: Option<u64>, limit: u32 }
Command::GetPassphraseAuditLatest
Command::WipeAllData
```

### New `CommandResult` variants

```rust
CommandResult::Config(ConfigSnapshot)
CommandResult::PassphraseChanged
CommandResult::Logs { records: Vec<LogRecord>, next_since_seq: u64 }
CommandResult::PassphraseAudit { last_changed_unix: Option<u64> }
// SetConfig + SetContactMuted + WipeAllData reuse existing CommandResult::Ok.
// (WipeAllData replies before tearing down; UI sees IPC close immediately after.)
```

### New supporting types

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigSnapshot {
    pub history_retention_days: u32,
    pub direct_timeout_secs: u32,
    pub notification_mode: NotificationMode,
    pub close_to_tray: bool,
    pub start_minimised: bool,
    pub persist_logs_to_disk: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigPatch {
    #[serde(default)] pub history_retention_days: Option<u32>,
    #[serde(default)] pub direct_timeout_secs: Option<u32>,
    #[serde(default)] pub notification_mode: Option<NotificationMode>,
    #[serde(default)] pub close_to_tray: Option<bool>,
    #[serde(default)] pub start_minimised: Option<bool>,
    #[serde(default)] pub persist_logs_to_disk: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum NotificationMode { Full, Minimal, Generic, Off }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct LogRecord {
    pub seq: u64,
    pub ts_unix_ms: u64,
    pub level: LogLevel,            // Trace | Debug | Info | Warn | Error
    pub target: String,             // e.g. "skattr_core::delivery::hub"
    pub message: String,            // already-redacted payload
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel { Trace, Debug, Info, Warn, Error }
```

### Additive fields on existing types

```rust
ContactSummary {
    // ... existing fields unchanged ...
    #[serde(default)] pub muted: bool,
    #[serde(default)] pub peer_mailboxes: Vec<String>,
}
```

### New `Event` variant + filter

```rust
Event::LogRecord(LogRecord)
EventFilter::Logs                       // delivered only when subscribed
```

### Mailbox CRUD — wire-shape unchanged

`Command::ListMailboxes` / `AddMailbox { onion }` / `RemoveMailbox { id }` keep their 2.C shapes; 2.F replaces the stub handler bodies with real wiring against `core::mailbox::client::MailboxClient`. The `MailboxSummary { id, onion, status, registered_at }` type is reused unchanged.

### `wire_format_append_only` snapshot test update

The static enumeration in `crates/core/tests/wire_format_append_only.rs` is appended with all new `Command` / `CommandResult` / `Event` variant names in the same commit as the Rust enum addition. The test fails until both halves match.

### ts-rs regeneration

`cargo test -p skattr-core` regenerates `crates/ui/src-svelte/src/lib/ipc/types/`. Generated TS is gitignored per 2.C decision 13. Hex types continue to serialise as bare lowercase hex strings (per 2.E lock).

## Storage migrations

### `0013_contacts_muted.sql`

```sql
ALTER TABLE contacts ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;
```

`Event::ContactUpdated` (existing from 2.E) is reused for mute toggles — no new event variant needed.

### `0014_passphrase_audit.sql`

```sql
CREATE TABLE IF NOT EXISTS passphrase_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_unix     INTEGER NOT NULL,
    outcome     TEXT NOT NULL CHECK(outcome IN ('changed','rolled_back','recovered'))
);
```

Append-only audit table; surfaces "Last changed" in Settings → Identity. Rows are not deleted by the retention sweep.

### Why the re-key journal is NOT in SQLite

The journal lives at `${data_dir}/passphrase-rekey.journal`, not as a SQLite row, because:

1. The age key being re-encrypted is the key that wraps the SQLite database itself. If the journal lived in SQLite, recovery on next boot would need to read SQLite → which needs the age key → which is the file we're recovering. Chicken-and-egg.
2. The two files being re-encrypted (`identity.vault`, `age-key`) are siblings of the journal in `${data_dir}`; keeping the journal next to them keeps the whole atomic operation in one directory.
3. The journal is tiny (≤ 256 bytes) — a file is the right tool.

## ChangePassphrase — journaled atomic re-key

### Files on disk

| File                       | Mode | Contents                                                                          |
|----------------------------|------|-----------------------------------------------------------------------------------|
| `identity.vault`           | 0600 | Argon2id-wrapped Ed25519 seed (existing, Phase 0.B)                               |
| `age-key`                  | 0600 | The `age` recipient secret that decrypts the SQLite database (existing, 0.D)     |
| `identity.vault.staged`    | 0600 | Re-encrypted identity vault under the NEW passphrase (new in 2.F; transient)     |
| `age-key.staged`           | 0600 | Re-encrypted age key under the NEW passphrase (new in 2.F; transient)            |
| `passphrase-rekey.journal` | 0600 | Present only during a re-key (new in 2.F; transient)                             |

Both `identity.vault` and `age-key` are wrapped using the **same** passphrase but with independent salts and independent KDF outputs (KEK-for-vault and KEK-for-age-key are derived via domain-separated HKDF labels — `"skattr-vault-v1"` and `"skattr-age-key-v1"`).

### Why stage-then-rename (not in-place + rollback-from-backup)

The naive "back up the old file, rewrite, restore on failure" approach has a recovery problem: if we crash between rewriting `identity.vault` and rewriting `age-key`, recovery on next boot needs to know which passphrase to use, and we can't tell without prompting the user for *both* old and new passphrases. Stage-then-rename eliminates this: at boot we can probe each file's wrapping passphrase by trying to decrypt with the user's currently-typed passphrase, and complete whichever rename is missing — without needing the old passphrase at all.

### Journal format (CBOR)

```rust
// crates/core/src/daemon/passphrase.rs
#[derive(Debug, Serialize, Deserialize, ZeroizeOnDrop)]
struct RekeyJournal {
    version: u8,                    // = 1 in 2.F
    new_salt: [u8; 16],             // Argon2id salt for the NEW passphrase
    started_unix: u64,
}
```

The journal carries no `step` field — its mere presence signals "a re-key is mid-flight; consult the on-disk file fingerprints to decide what to do." The `new_salt` is needed only so that recovery can compute the NEW KEKs from the user's prompted passphrase.

Journal is written via temp-file + `fsync` + `rename` + parent-dir `fsync`. Same pattern for every other on-disk mutation in the flow.

### Happy-path sequence

```
 1. ChangePassphrase { old, new } arrives over IPC.
    Both are immediately wrapped in Zeroizing<String>.
 2. Validate `new`:
      - byte length ≥ 8
      - server-side zxcvbn ≥ 3 (matches first-run wizard)
      - != old (constant-time compare on the bytes)
    On any failure → DaemonErrorKind::InvalidArgument; no files touched.
 3. Verify `old` by decrypting identity.vault AND age-key with it.
    Cache the decrypted plaintexts in Zeroizing<…> locals.
    On failure → DaemonErrorKind::Unauthorized; no files touched.
 4. Generate new_salt (16 bytes from OsRng).
    Compute new_KEK_vault = HKDF(new, new_salt, "skattr-vault-v1")
            new_KEK_age   = HKDF(new, new_salt, "skattr-age-key-v1")
 5. Stage:
      a. Write identity.vault.staged: encrypt(decrypted_vault_plaintext, new_KEK_vault).
         Temp-file + fsync + rename into place. fsync parent dir.
      b. Write age-key.staged: encrypt(decrypted_age_key_plaintext, new_KEK_age).
         Temp-file + fsync + rename into place. fsync parent dir.
 6. Write passphrase-rekey.journal { version: 1, new_salt, started_unix: now() }.
    Temp-file + fsync + rename + parent-dir fsync.
 7. Atomic rename: identity.vault.staged → identity.vault. fsync parent dir.
 8. Atomic rename: age-key.staged → age-key. fsync parent dir.
 9. Delete passphrase-rekey.journal. fsync parent dir.
10. Append passphrase_audit row with outcome='changed'.
11. Refresh DaemonInbound::set_identity(Arc::new(new_identity)).
    (In-memory IdentityKey is unchanged — same Ed25519 seed; only the
    at-rest wrapping changed. The set_identity refresh is defensive
    symmetry with future seed-rotating flows.)
12. Reply CommandResult::PassphraseChanged.
```

Steps 2–11 are wrapped in a `Drop`-guard struct that, if dropped without explicit `commit()` (i.e. a panic or an early `?`), runs a best-effort cleanup: delete `*.staged` files and the journal. (The on-disk state is still consistent because the staging files were never renamed into place. Recovery on a future boot, if it sees a leftover journal, will follow the recovery flow below.) All passphrase bytes and decrypted plaintexts live in `Zeroizing<…>` end-to-end.

### Recovery on daemon boot

`Daemon::run` calls `passphrase::recover_if_needed(data_dir)` **before** prompting the user for a passphrase.

```
journal = read passphrase-rekey.journal
if absent:
    # No re-key in flight. Best-effort cleanup of any orphaned .staged files
    # (e.g. left over from a panic during step 5 before the journal was written).
    delete identity.vault.staged if present
    delete age-key.staged if present
    return Ok(())   # normal boot

# Journal exists → a re-key was interrupted somewhere between steps 6 and 9.
# We don't know yet which renames have happened. We need the user's CURRENT
# passphrase to probe.

prompt user for passphrase as normal ("Enter your passphrase").

# Probe identity.vault with the prompted passphrase.
identity_decrypts_with_prompted = try_decrypt(identity.vault, prompted)

# Probe with the prompted passphrase under the journal's new_salt
# (i.e. is identity.vault already wrapped under the NEW KEK for that salt?).
is_identity_new = identity_decrypts_with_prompted
                  using HKDF(prompted, journal.new_salt, "skattr-vault-v1")

# Same probe for age-key.
is_age_key_new = try_decrypt(age-key,
                             HKDF(prompted, journal.new_salt, "skattr-age-key-v1"))

match (is_identity_new, is_age_key_new):
  (true, true):
      # Both renames happened (steps 7 + 8). We just crashed before deleting
      # the journal (step 9). Finish the rename: nothing to do for files;
      # delete the journal + any leftover .staged. Audit('recovered').
      delete journal; delete *.staged if present; fsync parent dir
      audit_append('recovered')

  (true, false):
      # Step 7 happened (identity.vault is NEW), step 8 didn't (age-key is OLD).
      # We have age-key.staged on disk — finish the rename.
      if age-key.staged exists:
          atomic rename age-key.staged → age-key; fsync
      else:
          # Defensive: this shouldn't happen because we wrote the journal AFTER
          # both .staged files. If we're here, something is corrupt. Surface
          # an error and direct the user to docs/operations/passphrase-recovery.md.
          return Err(...)
      delete journal; fsync parent dir
      audit_append('recovered')

  (false, false):
      # Neither rename happened (we crashed between step 6 and step 7).
      # Both files are still OLD; the prompted passphrase decrypted neither
      # under the NEW KEK. Verify the prompted passphrase decrypts the OLD
      # identity.vault — if so, the user gave us the OLD passphrase; roll back.
      if try_decrypt(identity.vault, HKDF(prompted, OLD_salt_from_vault_header,
                                          "skattr-vault-v1")):
          delete *.staged; delete journal; fsync parent dir
          audit_append('rolled_back')
          # User continues unlocking with the OLD passphrase.
      else:
          # Prompted passphrase doesn't decrypt under OLD or NEW.
          # User typed wrong passphrase. Re-prompt; do not modify any files.
          return Err(WrongPassphrase)

  (false, true):
      # Logically impossible given the step order (we always rewrite identity
      # first). If observed, the on-disk state has been tampered with or
      # corrupted. Surface an error pointing at passphrase-recovery.md.
      return Err(InconsistentState)
```

Total disk overhead during the operation: ≤ 8 KiB (two small staged files + journal). Recovery is deterministic from on-disk fingerprints alone — no need to retain the OLD passphrase from the user.

### UX

```
Settings → Identity → [ Change passphrase ]
  ┌────────────────────────────────────────────┐
  │ Change passphrase                          │
  │                                            │
  │ Current passphrase:  [ ************    ]   │
  │ New passphrase:      [ ************    ]   │
  │ Confirm new:         [ ************    ]   │
  │ Strength:            ████░░  Strong         │
  │                                            │
  │            [ Cancel ]   [ Change ]         │
  └────────────────────────────────────────────┘
```

Client-side: zxcvbn meter live on the New field; "Change" disabled until strength ≥ 3 AND New == Confirm AND New != Current. On submit, the dialog shows a non-cancellable "Changing passphrase…" spinner — cancelling mid-operation is exactly what the journal handles, and the UI shouldn't suggest it's safe to cancel.

On `CommandResult::PassphraseChanged` → toast "Passphrase changed." On error: surface `DaemonErrorKind::Unauthorized` ("Current passphrase is wrong") or `InvalidArgument` ("New passphrase too weak / matches current") inline.

### Test plan

`crates/tests/src/passphrase_atomicity.rs`, gated on `feature = "test-harness"`. A new `pub(crate)` `KillSwitch` (mirroring `delivery::kill_stream::KillSwitch` from 1.E) sits inside `passphrase::rekey` and can be triggered to panic before each of these kill points:

| Kill point | Panics before…                          | Expected post-recovery passphrase | Expected file fingerprints                      |
|------------|------------------------------------------|------------------------------------|--------------------------------------------------|
| K1         | step 5a (write `identity.vault.staged`)  | OLD                                | both files OLD; no `.staged`; no journal         |
| K2         | step 5b (write `age-key.staged`)         | OLD                                | both files OLD; `identity.vault.staged` cleaned; no journal |
| K3         | step 6 (write journal)                   | OLD                                | both files OLD; both `.staged` cleaned; no journal |
| K4         | step 7 (rename `identity.vault.staged → identity.vault`) | OLD (rolled back via recovery) | both files OLD; both `.staged` deleted; journal deleted |
| K5         | step 8 (rename `age-key.staged → age-key`) | NEW (rolled forward via recovery) | both files NEW; `.staged` cleaned; journal deleted |
| K6         | step 9 (delete journal)                  | NEW (rolled forward via recovery) | both files NEW; `.staged` cleaned; journal deleted |

For each kill point:

1. Spin up a daemon, set passphrase to `"old"`.
2. Send `ChangePassphrase { old: "old", new: "new" }` with the kill switch armed at point K.
3. Daemon panics → tokio task aborts.
4. Restart daemon; recovery runs with the prompted passphrase per the table.
5. Assert: daemon unlocks with the expected passphrase per the table.
6. Assert: file fingerprints match the table (probe each file's wrapping by attempting to decrypt under both old and new KDF outputs).
7. Assert: SQLite is readable, identity Ed25519 pubkey is unchanged.
8. Assert: `passphrase_audit` row exists with the expected outcome (`changed` for K6 only after a successful retry; `recovered` for K4/K5; `rolled_back` for K1–K3 if the user retries with the OLD passphrase).

For K4: also test that re-prompting with the NEW passphrase (the user "remembered" the new one) returns a clean `WrongPassphrase` error and leaves all files OLD until they retry with the actual OLD passphrase. For K5: the inverse — re-prompting with the OLD passphrase returns `WrongPassphrase` because identity.vault is already NEW.

### Documentation

`docs/operations/passphrase-recovery.md` (new in 2.F):

- What the journal does and why it exists.
- The on-disk file layout during a re-key (which files are transient, which are persistent).
- Manual recovery procedure for the rare `(false, true)` "logically impossible" branch (delete journal + `.staged` files; daemon will boot with OLD passphrase from `identity.vault` + `age-key`).
- "Lost passphrase" remains unrecoverable by design (the BIP39 seed is the only true recovery, and `restore` re-creates a new vault).

## Settings UI

### Shell — sidebar nav (`routes/settings/+layout.svelte`)

```
┌─────────────┬─────────────────────────────────────────┐
│ Settings  × │ Identity                                │
├─────────────┤                                          │
│ ▸ Identity  │ Public key:  abc123…  [Copy full]       │
│   Mailboxes │ Onion:       xyz…onion  [Copy full]     │
│   History   │ Card version: 7   Last changed: 2026-04-… │
│   Notif.    │                                          │
│   Advanced  │ [ Rotate onion ]   [ Change passphrase ]│
└─────────────┴─────────────────────────────────────────┘
```

Sidebar fixed at 200 px; content pane scrolls independently. ESC closes Settings, returns to `/`. Each route mounts its own component; first mount fires `GetConfig` (cached in the `config` store ~5 s to avoid round-trip on tab-flick). Saving any control fires `SetConfig { patch }` debounced 500 ms; inflight save shows a small spinner next to the field; success → checkmark fades after 1 s; error → inline message.

### Identity (`identity/+page.svelte`)

Reads `Command::DaemonInfo` (existing). Buttons:

- **Copy full** (pubkey/onion) — Tauri clipboard write; toast "Copied".
- **Rotate onion** — confirm dialog, then `Command::RotateOnion` (existing wire; bumps version + republishes current onion until Task 23.5 lands real HS rotation).
- **Change passphrase** — opens the dialog above.
- **Last changed** — `Command::GetPassphraseAuditLatest` → "Last changed: 2026-04-…" or "Never".

### Mailboxes (`mailboxes/+page.svelte`)

Real wiring against 2.B's `MailboxClient`. List rendered from `Command::ListMailboxes` → `MailboxSummary`. Each row shows onion (truncated) + status pill (`Reachable` / `Unreachable` / `Authenticated`) + registered date + remove button. "Add mailbox" opens a dialog with a single "Onion address" input; submit fires `Command::AddMailbox { onion }`. Live updates via `Event::MailboxStatusChanged` (existing from 2.B).

Stub handlers in `daemon::dispatch` are replaced with bodies that call into `core::mailbox::client::MailboxClient::register` / `deregister` / list-from-`mailboxes`-table. Errors surface as `DaemonErrorKind::Mailbox(MailboxErrorKind)` (existing).

### History (`history/+page.svelte`)

```
History
─────────────────────────────────────────────────────
Retention:  ( ) 24 hours
            ( ) 7 days
            ( ) 30 days
            ( ) 90 days
            (•) Never delete

Search:     [ ⌘K  Open search palette ]
            (Or use Ctrl/Cmd+K from anywhere)

Export:     ( ) JSONL  (•) Plaintext   [ ] gzip
            [ Download all conversations ]

Prune:      [ Delete messages older than … ]   ← opens confirm dialog
```

- **Retention** maps to `ConfigPatch.history_retention_days` ∈ `{ 1, 7, 30, 90, 0 }` (0 = Never). The retention sweep at `crates/core/src/daemon/retention.rs` already honours this.
- **Search** button mounts `<SearchPalette inline />` below the row when clicked (same component used in the Cmd/Ctrl-K modal, just rendered inline).
- **Export** uses 1.G's `Command::ExportHistory`; the format dropdown + gzip checkbox are client-side post-processing (the daemon returns CBOR rows; the Tauri command writes JSONL or formatted plaintext to the chosen file). Filename defaults per decision 10.
- **Prune** uses 1.G's `Command::PruneHistory`. Confirm dialog: "Delete N messages older than T? This cannot be undone."

### Notifications (`notifications/+page.svelte`)

```
Notifications
─────────────────────────────────────────────────────
Mode:           (•) Full       Sender + message preview
                ( ) Minimal    Sender only
                ( ) Generic    "New message"
                ( ) Off        No notifications

Behaviour:      [×] Suppress when window is focused AND
                    the active conversation receives the message
                    (Other conversations still notify.)

Per-conversation mute is on each contact's details panel.
```

`mode` → `ConfigPatch.notification_mode`. The "behaviour" checkbox is hard-locked (decision 4) — rendered as a non-interactive informational row with a tooltip explaining it.

Notification dispatcher (`lib/Notifications/dispatcher.ts`) subscribes to `Event::MessageReceived`. Truth table:

```ts
function shouldNotify(evt, focus, config, contact): boolean {
  if (config.notification_mode === 'off') return false;
  if (contact.muted) return false;
  if (focus.windowFocused && focus.activeContactId === evt.contact) return false;
  return true;
}
```

If notify: build title/body per `notification_mode` and `invoke('notify', { title, body, conversation_id })` → Rust-side `notify_rust::Notification::new(...).show()`. Click action → `invoke('focus_window_and_open_conversation', { id })`.

### Advanced (`advanced/+page.svelte`)

```
Advanced
─────────────────────────────────────────────────────
Logs:           [ Open logs viewer ▾ ]   ← expands inline
                [×] Persist logs to disk (rotated daily, 7-day retention)

Debug info:     Daemon version:  0.1.0
                Schema version:  14
                Tor:             v0.4.x.y via Arti 0.41
                Data dir:        /home/.../skattr  [Copy]
                IPC socket:      /run/user/1000/skattr/daemon.sock  [Copy]

Danger zone:    [ Delete all data and quit ]
                ↑ confirm-once-then-confirm-twice; daemon shuts down,
                  data_dir is removed, app exits.
```

**Logs viewer** — when expanded, subscribes via `EventFilter::Logs` and runs `Command::TailLogs` for backfill. Renders ≤ 500 most-recent records; auto-scrolls unless the user has scrolled up. Records colour-coded by level (`--text-muted` for trace/debug, `--text` for info, `--accent` for warn, `--danger` for error). "Copy logs" button copies the visible buffer.

**Persist logs to disk** — toggling fires `SetConfig { persist_logs_to_disk }`. Daemon-side: a `tracing-subscriber::reload::Layer` swaps in/out a `tracing-appender::rolling::daily` writer pointed at `${data_dir}/logs/skattr.log`. Off-toggle does NOT delete already-persisted files (documented in the toggle's tooltip).

**Danger zone — Delete all data and quit** — `Command::WipeAllData` (new variant). Daemon flow:

1. Reply `CommandResult::Ok` immediately.
2. Stop accepting new IPC connections.
3. Drop `Pool` (closes SQLite cleanly).
4. `tokio::fs::remove_dir_all(data_dir).await`.
5. `std::process::exit(0)`.

UI catches the IPC connection close and shows "Skattr has wiped all data and shut down. You can close this window."

### Tray + close-button (`crates/ui/src/tray.rs`, `main.rs`)

Tauri 2 built-in tray (decision 6). Menu structure:

```
Skattr                  (header, disabled)
─────────────────────
Show window
─────────────────────
Tor: <status>           (disabled, status only)
Unread: <n>             (disabled, status only; hidden when 0)
─────────────────────
Quit
```

Click tray icon → toggle window show/hide. Status items refresh on `TorStatusChanged` and on contact-list mutations (each `Event::MessageReceived` for a non-muted contact bumps unread). Tray badge dot (red overlay on the tray icon) shown when total unread > 0.

**Close-button semantics:** `tauri::WindowEvent::CloseRequested` is intercepted; if `config.close_to_tray` (default `true`), `event.prevent_close()` + `window.hide()`. First time per install, a toast: "Skattr is still running in the tray. Quit from the tray menu to stop the daemon." A localStorage flag (`shown_close_to_tray_toast: true`) suppresses subsequent toasts. Quit from tray = `app.exit(0)`, drops the Tauri main process, drops `Daemon::run`, trips its shutdown future cleanly.

`config.start_minimised` (default `false`): consulted in `main.rs` after `Daemon::run`'s ready signal — if true, the main window is created hidden.

### Cmd/Ctrl-K search palette (`SearchPalette.svelte`)

Globally registered shortcut in `+layout.svelte`'s root: `Cmd+K` (macOS) / `Ctrl+K` (Linux/Windows) opens the modal. Same component mounted inline in Settings → History; in modal mode it gets `position: fixed; inset: 10vh 20vw;` styling and a focus trap.

```
┌─────────────────────────────────────────────────────────┐
│  🔍 Search messages                                  ESC│
├─────────────────────────────────────────────────────────┤
│  [ tuesday at noon                              ]       │
├─────────────────────────────────────────────────────────┤
│  Alice · 3 days ago                                     │
│  ...how about we meet on >tuesday< at noon?             │
│                                                         │
│  Bob · 1 week ago                                       │
│  Did you see >tuesday<'s update?                        │
└─────────────────────────────────────────────────────────┘
```

Implementation: input debounced 200 ms; each query fires `Command::SearchMessages { query, limit: 50, offset: 0 }` (existing from 1.G; no wire change). Results render with FTS5 `snippet()` highlights (existing wire payload carries snippet markup with `>...<` markers). Click a row → `goto('/conversation/' + contact + '?focus_row_id=' + row.row_id)`.

`routes/conversation/[contact]/+page.svelte` is extended to read `focus_row_id`: scrolls to and briefly highlights that row, **does not advance the read cursor** (cursor only advances on bottom-of-list intersection — 2.D semantics preserved).

Cmd/Ctrl-K state lives in a small `searchPalette` store: `{ open: boolean, query: string, results: MessageRecord[] }`. ESC closes; arrow keys navigate; Enter opens highlighted row.

### Contact details panel — mute toggle + `peer_mailboxes`

`crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte` (from 2.E) gains:

- A bell icon next to the contact's nickname → toggles `Command::SetContactMuted`. Persists across reboots (daemon-side via `contacts.muted`).
- The "Mailboxes" section that shows "No mailboxes" today renders `contact.peer_mailboxes` as a small list (each onion truncated to `xyz…1234.onion`, click-to-copy full).

Mute icon also appears next to muted contacts in the main contact list (left rail).

## Test plan

| Scope                | Test                                                                                  | File                                                              |
|----------------------|---------------------------------------------------------------------------------------|-------------------------------------------------------------------|
| Wire-format          | New variants are appended; rejected reshapes                                          | `crates/core/tests/wire_format_append_only.rs` (existing, updated)|
| Config round-trip    | `GetConfig` → `SetConfig` → `GetConfig` matches                                       | `crates/tests/src/settings_roundtrip.rs` (new)                    |
| Passphrase atomicity | Six kill-points (K1–K6) per the table in §"Test plan" under ChangePassphrase         | `crates/tests/src/passphrase_atomicity.rs` (new)                  |
| Mailbox CRUD wiring  | Add → list → remove against a real 2.B mailbox client                                 | `crates/tests/src/mailbox_crud.rs` (new)                          |
| Mute persistence     | Mute → restart daemon → mute survives                                                 | `crates/core/src/contact/repo.rs` unit + integration              |
| `peer_mailboxes`     | Latest verified card's mailboxes appear in `ContactSummary`                           | `crates/core/src/contact/repo.rs` unit                            |
| Notification logic   | `shouldNotify(...)` truth table over (mode × focus × active × muted)                  | `crates/ui/src-svelte/src/lib/Notifications/dispatcher.test.ts`   |
| Search palette       | Cmd/Ctrl-K opens; query → results → click navigates + scrolls + does NOT mark-read   | Vitest + Playwright                                               |
| Tray + close-to-tray | Window hides on close when toggle on; quit from tray menu exits process               | Manual + Vitest for the toast suppression flag                    |
| Logs viewer          | `Event::LogRecord` flows when `EventFilter::Logs`; redaction layer drops onions      | `crates/core/src/daemon/logs.rs` unit                             |
| Wipe-all-data        | `WipeAllData` removes `data_dir`, exits 0; restart re-runs first-run wizard           | `crates/tests/src/wipe_data.rs` (new)                             |
| Per-OS notifications | Manual checklist on Linux / macOS / Windows                                           | `docs/operations/2f-notification-smoke.md`                        |

## Exit criterion

Per the umbrella decomposition (`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` §2.F):

> All settings round-trip; mailbox CRUD wired against 2.B; passphrase change works without data loss; notifications respect focus + mute.

Concretely:

- [ ] All 13 locked decisions implemented per this spec.
- [ ] `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --features test-harness` green on Linux + macOS CI.
- [ ] `cargo deny check` clean (any new dep — `notify-rust`, `tracing-appender`, `tracing-subscriber` reload feature, `zxcvbn` server-side — passes the allowlist).
- [ ] Six kill-point passphrase atomicity tests green; SQLite remains readable + identity pubkey unchanged in every scenario.
- [ ] Manual cross-OS notification smoke checklist (`docs/operations/2f-notification-smoke.md`) marked complete on at least Linux (the dev environment); macOS/Windows runs deferred to whoever has access.
- [ ] CHANGELOG bullet committed.
- [ ] CLAUDE.md "Repository state" section updated to reflect 2.F merge.

## Out of scope (carry-forward)

- **Task 2.E.5** (mailbox fallback for Welcome) — independent follow-up; may slot in if scope allows but not required for 2.F exit.
- **Task 20.5** (peer direct-timeout trigger from `PeerConnection` to `DeliveryHub::ensure_mailbox_fallback`).
- **Task 22.5** (route `RemoveMailbox`'s final-drain ciphertexts through `DaemonInbound::dispatch`).
- **Task 23.5** (real HS key rotation — `RotateOnion` today only bumps the self-card version).
- "Restore archived contact" UI (Settings → Contacts → Archived) — not scoped unless trivial.
- Wire-format BREAKING changes — anything renaming or removing a `Command` / `CommandResult` / `Event` variant requires a separate spec.
- Phase 2.G (packaging & distribution).
- Phase 3+ items: avatars, reactions, replies, edits, typing indicators, attachments, multi-member groups.
- Phase 4+ items: cover traffic, panic-wipe, duress mode.
- Phase 5+ items: auto-update, code signing + notarisation.

## Risks and mitigations

| Risk                                                                        | Mitigation                                                                                                              |
|-----------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `ChangePassphrase` corrupts a vault despite the journal                     | Six kill-point tests (K1–K6) cover every transition; recovery is deterministic from on-disk fingerprints; `docs/operations/passphrase-recovery.md` documents the manual recovery procedure for the "logically impossible" branch. |
| `notify-rust` per-OS quirks block 2.F merge                                 | Rust integration test asserts only `.show()` returns `Ok(_)`; cross-OS rendering is a manual checklist, not a CI gate.   |
| Tauri 2 tray API surprise on Linux (Wayland / DBus / no-tray environments)  | Guard tray init in `main.rs`; if tray creation fails, log a warning and fall back to "no tray" mode (close = quit). Documented. |
| `tracing-subscriber::reload::Layer` adds runtime overhead even when disabled | Benchmark before merge; if cost is non-trivial, gate the disk-persist option behind a `cfg` and require restart to enable. |
| `WipeAllData` race with in-flight IPC clients                               | Daemon stops accepting new connections immediately; existing connections are dropped when `Pool` drops; CLI prints a clean error on connection close. |
| Search palette navigates and accidentally advances read cursor              | Conversation view's mark-read is gated on bottom-of-list intersection only; `focus_row_id` deep-link does NOT trigger that gate. Explicit test asserts this. |

## Wire-format snapshot summary

After 2.F, the snapshot enumeration in `crates/core/tests/wire_format_append_only.rs` includes:

- **New `Command` names**: `GetConfig`, `SetConfig`, `ChangePassphrase`, `SetContactMuted`, `TailLogs`, `GetPassphraseAuditLatest`, `WipeAllData`.
- **New `CommandResult` names**: `Config`, `PassphraseChanged`, `Logs`, `PassphraseAudit`.
- **New `Event` names**: `LogRecord`.
- **New `EventFilter` names**: `Logs`.

All other variants in those enums remain present and unchanged. New fields on existing types (`ContactSummary.muted`, `ContactSummary.peer_mailboxes`) carry `#[serde(default)]` and do not break the snapshot test.
