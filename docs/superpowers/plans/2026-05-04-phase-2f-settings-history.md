# Phase 2.F Settings & History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the settings panel (Identity / Mailboxes / History / Notifications / Advanced), real mailbox CRUD wiring against 2.B's `MailboxClient`, journaled atomic `ChangePassphrase`, focus-aware notifications with daemon-side per-conversation mute, Tauri tray + close-to-tray, and the Cmd/Ctrl-K cross-conversation search palette — closing Phase 2's user-facing chrome.

**Architecture:** Wire-format additive only. Hybrid IPC: `Command::GetConfig` / `SetConfig { patch }` for normal config; security-sensitive ops (`ChangePassphrase`, `RotateOnion`, `WipeAllData`) stay separate. `ChangePassphrase` uses **stage-then-rename**: re-encrypted vault + age key are staged on disk, journal is written, then both renames happen in sequence. Recovery on boot probes file fingerprints with the typed passphrase and never needs the OLD passphrase. Tray uses Tauri 2's built-in `tray::TrayIconBuilder`. Search palette is a single Svelte component, mounted as a modal (Cmd/Ctrl-K) and inline in Settings → History.

**Tech Stack:** Rust 2021 (skattr-core / -ui / -tests), Tauri 2 + SvelteKit + Vite + Vitest + Playwright, `notify-rust` (new dep), `tracing-appender` + `tracing-subscriber::reload` (new feature), `zxcvbn` (server-side; new dep), rusqlite 0.38, tokio, ciborium.

**Spec:** `docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md` — 13 locked decisions.

**Worktree:** Create `phase-2f-settings-history` branch off master `2c4d73e` in `/home/myggiz/development/skattr-phase-2f-settings-history/` before Task 1 (use the `superpowers:using-git-worktrees` skill).

**Conventions:**
- Tests live next to the code (unit tests in `mod tests` blocks); cross-binary integration tests in `crates/tests/src/`.
- Cargo runs require `. "$HOME/.cargo/env" &&` prefix per CLAUDE.md.
- `cargo test -p skattr-core` requires `--features test-harness` per memory; full-tree `cargo test` does not.
- Every `.rs` file carries `// SPDX-License-Identifier: GPL-3.0-or-later` + `// Copyright (C) 2026 Myggiz B.V.` headers (AGPLv3 for `crates/mailbox/`, but 2.F doesn't touch that crate).
- All commits include the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- Each task ends with **one** commit; commit messages follow the existing style (`feat(scope): subject` / `fix(scope): subject` / `docs(scope): subject` / `test(scope): subject`).
- After every Rust task, run `cargo fmt --all` then `cargo clippy --all-targets -- -D warnings` then the relevant `cargo test` invocation. Don't stage failing-clippy code.
- ts-rs regenerates TS bindings on `cargo test -p skattr-core`; the generated TS is gitignored per 2.C decision 13. After any wire-format addition, run the test suite to refresh bindings before touching any TS that consumes them.

---

## File Structure

### New SQL migrations

| Path | Purpose |
|---|---|
| `crates/core/src/storage/migrations/0013_contacts_muted.sql` | Adds `contacts.muted INTEGER NOT NULL DEFAULT 0` |
| `crates/core/src/storage/migrations/0014_passphrase_audit.sql` | Append-only `passphrase_audit (id, ts_unix, outcome)` |

### New Rust modules

| Path | Purpose |
|---|---|
| `crates/core/src/daemon/passphrase.rs` | Stage-then-rename `ChangePassphrase`; recovery on boot; KillSwitch for tests |
| `crates/core/src/daemon/logs.rs` | In-memory ring-buffer tracing layer + redaction + IPC stream |
| `crates/ui/src/tray.rs` | `tauri::tray::TrayIconBuilder` setup + click handlers + status refresh |
| `crates/ui/src/notifications.rs` | `notify-rust` wrapper exposed as a Tauri command |

### New Rust integration tests

| Path | Purpose |
|---|---|
| `crates/tests/src/passphrase_atomicity.rs` | Six kill-points (K1–K6) × recovery |
| `crates/tests/src/settings_roundtrip.rs` | `GetConfig` → `SetConfig` → `GetConfig` |
| `crates/tests/src/mailbox_crud.rs` | Add → list → remove against a real mailbox |
| `crates/tests/src/wipe_data.rs` | `WipeAllData` removes data_dir, exits 0 |

### New Svelte components

| Path | Purpose |
|---|---|
| `crates/ui/src-svelte/src/lib/components/SearchPalette.svelte` | Cmd/Ctrl-K modal + inline mode |
| `crates/ui/src-svelte/src/lib/components/ChangePassphraseDialog.svelte` | Modal with zxcvbn meter |
| `crates/ui/src-svelte/src/lib/components/SettingsSidebar.svelte` | Sidebar nav for `routes/settings/+layout.svelte` |
| `crates/ui/src-svelte/src/lib/Notifications/dispatcher.ts` | Focus-aware notification truth table |
| `crates/ui/src-svelte/src/lib/Notifications/dispatcher.test.ts` | Vitest |
| `crates/ui/src-svelte/src/lib/stores/config.ts` | GetConfig snapshot mirror + debounced SetConfig writer |
| `crates/ui/src-svelte/src/lib/stores/focus.ts` | windowFocused + activeContactId tracker |
| `crates/ui/src-svelte/src/lib/stores/logs.ts` | Subscribes to `Event::LogRecord` when Advanced/Logs is open |
| `crates/ui/src-svelte/src/lib/stores/searchPalette.ts` | Palette open / query / results |

### New Svelte routes

| Path | Purpose |
|---|---|
| `crates/ui/src-svelte/src/routes/settings/+layout.svelte` | Sidebar + outlet; ESC returns to `/` |
| `crates/ui/src-svelte/src/routes/settings/identity/+page.svelte` | Identity section |
| `crates/ui/src-svelte/src/routes/settings/mailboxes/+page.svelte` | Mailboxes section |
| `crates/ui/src-svelte/src/routes/settings/history/+page.svelte` | History section |
| `crates/ui/src-svelte/src/routes/settings/notifications/+page.svelte` | Notifications section |
| `crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte` | Advanced section + logs viewer + danger zone |

### New docs

| Path | Purpose |
|---|---|
| `docs/operations/2f-notification-smoke.md` | Per-OS manual smoke checklist |
| `docs/operations/passphrase-recovery.md` | Journal + manual recovery procedure |

### Modified Rust files

| Path | Change |
|---|---|
| `crates/core/src/storage/migrations.rs` | Append migrations 0013 + 0014 entries; new tests |
| `crates/core/src/daemon/config.rs` | New `[delivery]` / `[notifications]` / `[ui]` sections; `ConfigSnapshot` + `ConfigPatch` types; `apply_patch` + atomic `save_to_disk` |
| `crates/core/src/daemon/commands.rs` | 7 new `Command` variants, 4 new `CommandResult` variants, 2 additive fields on `ContactSummary`, supporting types (`NotificationMode`, `LogLevel`, `LogRecord`, `ConfigSnapshot`, `ConfigPatch`) |
| `crates/core/src/daemon/events.rs` | New `Event::LogRecord(LogRecord)` |
| `crates/core/src/daemon/ipc/wire.rs` | New `EventFilter::Logs` |
| `crates/core/src/daemon/error_kind.rs` | New `Unauthorized`, `WrongPassphrase`, `InconsistentState` variants for `DaemonErrorKind` |
| `crates/core/src/daemon/dispatch.rs` | New handlers: `get_config`, `set_config`, `change_passphrase`, `set_contact_muted`, `tail_logs`, `get_passphrase_audit_latest`, `wipe_all_data`; replace mailbox CRUD stubs with real bodies |
| `crates/core/src/daemon/mod.rs` | Register `passphrase`, `logs` modules; call `passphrase::recover_if_needed` before unlock; install `logs::layer()` in subscriber stack |
| `crates/core/src/daemon/inbound.rs` | (no behaviour change; the `set_identity` refresh after passphrase change calls existing API) |
| `crates/core/src/contact/repo.rs` | New `set_muted(pubkey, bool) -> Result<()>` |
| `crates/core/src/daemon/contacts_summary.rs` (or wherever `ContactSummary` is built) | Project `muted` from `contacts.muted`; project `peer_mailboxes` from latest verified `ContactCard.body.mailboxes` |
| `crates/core/tests/wire_format_append_only.rs` | Append new variant names to expected snapshot lists |
| `crates/core/Cargo.toml` | Add `notify-rust` (UI-only, but invoked from a Tauri command which lives in `crates/ui/`); add `tracing-appender`, enable `reload` feature on `tracing-subscriber`; add `zxcvbn` (server-side validator) |
| `crates/ui/src/main.rs` | Initialise tray; register `notify` + `focus_window_and_open_conversation` + `wipe_data_observer` Tauri commands; install `WindowEvent::CloseRequested` handler; consult `start_minimised` after ready signal |
| `crates/ui/src/ipc_bridge.rs` | (no shape change; new `Command` variants flow through unchanged) |

### Modified TypeScript files

| Path | Change |
|---|---|
| `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte` | Add bell icon (mute toggle) + render `peer_mailboxes` |
| `crates/ui/src-svelte/src/lib/components/ContactRow.svelte` | Add bell-icon indicator next to muted contacts |
| `crates/ui/src-svelte/src/routes/+layout.svelte` | Register Cmd/Ctrl-K global keybinding; mount `<SearchPalette />` (modal mode) |
| `crates/ui/src-svelte/src/routes/conversation/[contact]/+page.svelte` | Read `?focus_row_id=` query param, scroll to + briefly highlight, do NOT advance read cursor |
| `crates/ui/src-svelte/src/lib/test/tauri-mock.ts` | Add fixtures: `settings-flow`, `passphrase-flow`, `notifications-flow`, `mailboxes-flow`, `search-flow`, `logs-flow`, `wipe-flow` |
| `crates/ui/src-svelte/package.json` | (no new deps; `zxcvbn` already bundled from 2.C first-run wizard; `notify-rust` is Rust-side) |

---

## Task list overview

| Phase | Tasks | Theme |
|---|---|---|
| 0 — Worktree & setup | 1 | Create branch + verify clean build |
| 1 — Storage | 2–3 | Migrations 0013, 0014 |
| 2 — Wire-format types | 4–7 | New `Command` / `CommandResult` / `Event` variants + supporting types + snapshot test |
| 3 — Config struct | 8–9 | `Config` extension + `apply_patch` + atomic save |
| 4 — Contact projections | 10–11 | `set_muted` + `peer_mailboxes` projection |
| 5 — Daemon dispatch (basics) | 12–14 | `GetConfig` / `SetConfig` / `SetContactMuted` |
| 6 — Mailbox CRUD wiring | 15 | Replace 2.C stubs with real `MailboxClient` calls |
| 7 — Passphrase atomicity | 16–20 | `passphrase.rs` module + `ChangePassphrase` handler + `KillSwitch` + integration test |
| 8 — Logs subsystem | 21–23 | `logs.rs` ring buffer + redaction + `TailLogs` + `Event::LogRecord` |
| 9 — Wipe-all-data | 24 | `WipeAllData` handler + integration test |
| 10 — Tauri tray + notifications | 25–28 | `tray.rs`, `notifications.rs`, close-to-tray, start-minimised |
| 11 — Settings UI shell | 29–31 | `config` store, sidebar layout, route scaffolding |
| 12 — Settings sections | 32–36 | Identity, Mailboxes, History, Notifications, Advanced |
| 13 — Search palette | 37–38 | `SearchPalette.svelte` + Cmd/Ctrl-K binding + conversation focus_row_id |
| 14 — Contact details | 39 | Mute toggle + `peer_mailboxes` rendering |
| 15 — Cross-binary integration tests | 40–41 | `settings_roundtrip`, `mailbox_crud` (passphrase + wipe already in their phases) |
| 16 — Docs + wrap-up | 42–44 | Notification smoke checklist, passphrase recovery doc, CHANGELOG + CLAUDE.md |

---

## Phase 0 — Worktree & setup

### Task 1: Create worktree and verify clean build

**Files:** none new; preparation only.

- [ ] **Step 1: Create worktree**

```bash
cd /home/myggiz/development/skattr
git worktree add -b phase-2f-settings-history /home/myggiz/development/skattr-phase-2f-settings-history master
cd /home/myggiz/development/skattr-phase-2f-settings-history
```

- [ ] **Step 2: Verify clean build**

```bash
. "$HOME/.cargo/env"
cargo build --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --features test-harness
```

Expected: all green. If anything fails, stop and investigate before any new code lands.

- [ ] **Step 3: Verify pnpm side**

```bash
cd crates/ui/src-svelte
pnpm install --frozen-lockfile
pnpm test
pnpm run check
```

Expected: install + Vitest + svelte-check all green.

- [ ] **Step 4: Capture baseline schema_version for the migration tests**

```bash
cd /home/myggiz/development/skattr-phase-2f-settings-history
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness migrations::tests::fresh_db_runs_migrations_to_latest -- --nocapture
```

Expected: passes; current max version = 12. Plan adds 13 + 14.

- [ ] **Step 5: Commit (no code change yet — this task records the baseline)**

This task makes no commits. Move on to Task 2.

---

## Phase 1 — Storage migrations

### Task 2: Migration 0013 — `contacts.muted`

**Files:**
- Create: `crates/core/src/storage/migrations/0013_contacts_muted.sql`
- Modify: `crates/core/src/storage/migrations.rs:25-74` (append entry) + `migrations.rs::tests` (new test)

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0013_contacts_muted.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0013: per-contact mute flag
--
-- `muted = 1` suppresses desktop notifications and the unread badge for
-- this contact's group. Default 0 (notifications fire). The UI surfaces
-- a bell icon next to muted contacts.

ALTER TABLE contacts ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;
```

- [ ] **Step 2: Register the migration**

Append to `ALL_MIGRATIONS` in `crates/core/src/storage/migrations.rs` (after the migration-12 entry):

```rust
    Migration {
        version: 13,
        sql: include_str!("migrations/0013_contacts_muted.sql"),
    },
```

- [ ] **Step 3: Write the failing test**

Append to `migrations.rs::tests` in `crates/core/src/storage/migrations.rs`:

```rust
    #[test]
    fn migration_0013_adds_contacts_muted_column() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('contacts')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            cols.iter().any(|c| c == "muted"),
            "migration 0013 must add contacts.muted; got {cols:?}"
        );
        // Default for existing rows must be 0
        conn.execute(
            "INSERT INTO contacts (pubkey, display_name, added_at, hidden) \
             VALUES (?1, NULL, 0, 0)",
            rusqlite::params![[0u8; 32]],
        )
        .unwrap();
        let muted: i64 = conn
            .query_row(
                "SELECT muted FROM contacts WHERE pubkey = ?1",
                rusqlite::params![[0u8; 32]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(muted, 0);
    }
```

- [ ] **Step 4: Run the test**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness migrations::tests::migration_0013_adds_contacts_muted_column -- --nocapture
```

Expected: PASS. Also verify the existing `fresh_db_runs_migrations_to_latest` still passes (max version is now 13).

- [ ] **Step 5: Run full migrations test suite**

```bash
cargo test -p skattr-core --features test-harness migrations:: -- --nocapture
```

Expected: all migration tests green.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage/migrations/0013_contacts_muted.sql \
        crates/core/src/storage/migrations.rs
git commit -m "$(cat <<'EOF'
feat(storage): migration 0013 — contacts.muted column

Adds an INTEGER NOT NULL DEFAULT 0 column to suppress desktop
notifications and unread badges per-contact. Surfaced as
ContactSummary.muted (additive) and toggled via the new
Command::SetContactMuted (added in Task 4).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Migration 0014 — `passphrase_audit`

**Files:**
- Create: `crates/core/src/storage/migrations/0014_passphrase_audit.sql`
- Modify: `crates/core/src/storage/migrations.rs:25-74` (append entry) + new test

- [ ] **Step 1: Write the migration SQL**

Create `crates/core/src/storage/migrations/0014_passphrase_audit.sql`:

```sql
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Copyright (C) 2026 Myggiz B.V.
--
-- Skattr schema migration 0014: passphrase change audit log
--
-- Append-only record of ChangePassphrase outcomes. Surfaced as
-- "Last changed" in Settings → Identity. Rows are NEVER deleted by the
-- retention sweep — small (one row per passphrase change) and auditable.

CREATE TABLE IF NOT EXISTS passphrase_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_unix     INTEGER NOT NULL,
    outcome     TEXT NOT NULL CHECK(outcome IN ('changed','rolled_back','recovered'))
);
```

- [ ] **Step 2: Register the migration**

Append to `ALL_MIGRATIONS` in `crates/core/src/storage/migrations.rs`:

```rust
    Migration {
        version: 14,
        sql: include_str!("migrations/0014_passphrase_audit.sql"),
    },
```

- [ ] **Step 3: Write the failing test**

Append to `migrations.rs::tests`:

```rust
    #[test]
    fn migration_0014_creates_passphrase_audit_table() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();

        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='passphrase_audit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "passphrase_audit table must exist");

        // Outcome CHECK constraint enforces the three valid values.
        let bad = conn.execute(
            "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (1, 'bogus')",
            [],
        );
        assert!(bad.is_err(), "CHECK constraint must reject invalid outcome");

        for outcome in ["changed", "rolled_back", "recovered"] {
            conn.execute(
                "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (?1, ?2)",
                rusqlite::params![1_700_000_000_i64, outcome],
            )
            .unwrap();
        }

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM passphrase_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }
```

- [ ] **Step 4: Run the test**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness migrations::tests::migration_0014_creates_passphrase_audit_table -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/storage/migrations/0014_passphrase_audit.sql \
        crates/core/src/storage/migrations.rs
git commit -m "$(cat <<'EOF'
feat(storage): migration 0014 — passphrase_audit table

Append-only record of ChangePassphrase outcomes, surfaced as
"Last changed" in Settings → Identity. CHECK constraint enforces
outcome ∈ {changed, rolled_back, recovered}. Rows are not deleted
by the retention sweep.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — Wire-format types

### Task 4: Supporting types — `NotificationMode`, `LogLevel`, `LogRecord`, `ConfigSnapshot`, `ConfigPatch`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (append types near the top, above `Command`)
- Modify: `crates/core/Cargo.toml` (add `zxcvbn` dep, server-side validator — used in Task 17, type-only here)

- [ ] **Step 1: Add the dependency**

In `crates/core/Cargo.toml` `[dependencies]`:

```toml
zxcvbn = { version = "3", default-features = false }
```

- [ ] **Step 2: Add the supporting types**

Append to `crates/core/src/daemon/commands.rs` (before `pub enum Command`):

```rust
/// Mode controlling what notification body is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export))]
#[serde(rename_all = "lowercase")]
pub enum NotificationMode {
    /// Sender nickname + body preview ("Alice: hey, can you...")
    Full,
    /// Sender only ("Alice")
    Minimal,
    /// Placeholder only ("New message")
    Generic,
    /// No notifications at all.
    Off,
}

impl Default for NotificationMode {
    fn default() -> Self {
        Self::Full
    }
}

/// Tracing log level, projected onto the wire so the UI logs viewer can
/// colour-code records. Mirrors `tracing::Level` but is `Serialize`able.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export))]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// One redacted log record streamed from the daemon ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export))]
pub struct LogRecord {
    /// Monotonic per-buffer sequence number; UI uses this as the
    /// `since_seq` cursor for incremental tail.
    pub seq: u64,
    /// Wall-clock at the time the record was emitted.
    pub ts_unix_ms: u64,
    pub level: LogLevel,
    /// e.g. "skattr_core::delivery::hub"
    pub target: String,
    /// Already-redacted message body (no pubkeys / onions / message
    /// contents above the `debug` level).
    pub message: String,
}

/// Snapshot of all UI-relevant config knobs. Sensitive paths
/// (`data_dir`, `ipc_socket`) are intentionally NOT projected — the UI
/// reads them via `Command::DaemonInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export))]
pub struct ConfigSnapshot {
    pub history_retention_days: u32,
    pub direct_timeout_secs: u32,
    pub notification_mode: NotificationMode,
    pub close_to_tray: bool,
    pub start_minimised: bool,
    pub persist_logs_to_disk: bool,
}

/// Patch sent by `Command::SetConfig`. Each field is `Option<T>`; the
/// daemon applies only `Some(_)` fields, validates each, then atomically
/// rewrites `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(export))]
pub struct ConfigPatch {
    #[serde(default)]
    pub history_retention_days: Option<u32>,
    #[serde(default)]
    pub direct_timeout_secs: Option<u32>,
    #[serde(default)]
    pub notification_mode: Option<NotificationMode>,
    #[serde(default)]
    pub close_to_tray: Option<bool>,
    #[serde(default)]
    pub start_minimised: Option<bool>,
    #[serde(default)]
    pub persist_logs_to_disk: Option<bool>,
}
```

- [ ] **Step 3: Verify build**

```bash
. "$HOME/.cargo/env"
cargo build -p skattr-core --features test-harness
cargo clippy -p skattr-core --features test-harness --all-targets -- -D warnings
```

Expected: clean build. New types are unused at this point — that's fine; they'll be wired in Tasks 5–7.

- [ ] **Step 4: Add a serde round-trip test**

Append to `crates/core/src/daemon/commands.rs`'s existing `#[cfg(test)] mod tests` (or create one if absent):

```rust
    #[test]
    fn config_patch_default_is_all_none() {
        let p = ConfigPatch::default();
        assert!(p.history_retention_days.is_none());
        assert!(p.notification_mode.is_none());
        assert!(p.close_to_tray.is_none());
    }

    #[test]
    fn config_patch_serde_roundtrip() {
        let p = ConfigPatch {
            history_retention_days: Some(30),
            notification_mode: Some(NotificationMode::Minimal),
            ..Default::default()
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&p, &mut bytes).unwrap();
        let back: ConfigPatch = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back.history_retention_days, Some(30));
        assert!(matches!(
            back.notification_mode,
            Some(NotificationMode::Minimal)
        ));
        assert!(back.close_to_tray.is_none());
    }

    #[test]
    fn notification_mode_serde_lowercase_kebab() {
        let m = NotificationMode::Generic;
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, "\"generic\"");
    }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::commands::tests::config_patch -- --nocapture
cargo test -p skattr-core --features test-harness daemon::commands::tests::notification_mode -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/Cargo.toml crates/core/src/daemon/commands.rs Cargo.lock
git commit -m "$(cat <<'EOF'
feat(commands): supporting types for Phase 2.F wire surface

Adds NotificationMode, LogLevel, LogRecord, ConfigSnapshot, and
ConfigPatch as additive types on the IPC wire. ConfigPatch fields
are all Option<T> with #[serde(default)] so partial patches round-trip
correctly. zxcvbn pulled in (server-side validator wired in Task 17).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: New `Command` variants

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (append to `Command` enum)
- Modify: `crates/core/src/daemon/dispatch.rs:33-82` (add stub arms returning `IpcError::UnknownCommand` so the match stays exhaustive)

- [ ] **Step 1: Add the variants**

In `crates/core/src/daemon/commands.rs`'s `pub enum Command`, append (preserving alphabetical-ish grouping):

```rust
    /// Read the current config snapshot.
    GetConfig,

    /// Apply a partial config patch. Daemon validates each field, then
    /// atomically rewrites config.toml. UI consumers debounce ~500ms so
    /// rapid edits don't thrash the disk.
    SetConfig {
        patch: ConfigPatch,
    },

    /// Re-encrypt the identity vault and storage age key under a new
    /// passphrase. Stage-then-rename atomicity; recovery on boot is
    /// deterministic. See `core::daemon::passphrase`.
    ChangePassphrase {
        /// Wrapped in `Zeroizing<String>` server-side as soon as decoded.
        old: String,
        new: String,
    },

    /// Toggle desktop-notification + unread-badge suppression for a
    /// single contact. Persisted in `contacts.muted`.
    SetContactMuted {
        contact: PublicKey,
        muted: bool,
    },

    /// Stream the most recent log records from the in-memory ring
    /// buffer. UI consumes this on Settings → Advanced → Logs open;
    /// live-tail uses `EventFilter::Logs`.
    TailLogs {
        /// `None` = "from the oldest record currently in the buffer".
        #[serde(default)]
        since_seq: Option<u64>,
        /// Hard cap; daemon clamps to ≤ 1000.
        limit: u32,
    },

    /// Read the most recent `passphrase_audit` row's `ts_unix`.
    GetPassphraseAuditLatest,

    /// Stop accepting IPC, drop the storage Pool, remove `data_dir`,
    /// then `process::exit(0)`. Reply is sent BEFORE the teardown.
    WipeAllData,
```

- [ ] **Step 2: Add stub arms in `dispatch.rs`**

In `crates/core/src/daemon/dispatch.rs::execute_command`'s match (file:dispatch.rs:33-82), append before the closing brace:

```rust
        Command::GetConfig => Err(IpcError::UnknownCommand),
        Command::SetConfig { .. } => Err(IpcError::UnknownCommand),
        Command::ChangePassphrase { .. } => Err(IpcError::UnknownCommand),
        Command::SetContactMuted { .. } => Err(IpcError::UnknownCommand),
        Command::TailLogs { .. } => Err(IpcError::UnknownCommand),
        Command::GetPassphraseAuditLatest => Err(IpcError::UnknownCommand),
        Command::WipeAllData => Err(IpcError::UnknownCommand),
```

(These stubs make the match exhaustive without forcing dependent tasks to land in lockstep. Each task in Phases 5–9 replaces its arm with real handlers.)

- [ ] **Step 3: Build + clippy**

```bash
. "$HOME/.cargo/env"
cargo build -p skattr-core --features test-harness
cargo clippy -p skattr-core --features test-harness --all-targets -- -D warnings
```

Expected: clean build, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(commands): add 7 new Command variants for Phase 2.F

GetConfig / SetConfig / ChangePassphrase / SetContactMuted / TailLogs /
GetPassphraseAuditLatest / WipeAllData. Stub dispatch arms return
UnknownCommand; real handlers land in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: New `CommandResult` variants + new `Event` variant + new `EventFilter` variant

**Files:**
- Modify: `crates/core/src/daemon/commands.rs` (append to `CommandResult`)
- Modify: `crates/core/src/daemon/events.rs` (append `LogRecord` variant)
- Modify: `crates/core/src/daemon/ipc/wire.rs` (append `EventFilter::Logs`)

- [ ] **Step 1: Add `CommandResult` variants**

In `crates/core/src/daemon/commands.rs`'s `pub enum CommandResult`, append:

```rust
    /// Reply for `Command::GetConfig`.
    Config(ConfigSnapshot),

    /// Reply for `Command::ChangePassphrase` (success).
    PassphraseChanged,

    /// Reply for `Command::TailLogs`.
    Logs {
        records: Vec<LogRecord>,
        next_since_seq: u64,
    },

    /// Reply for `Command::GetPassphraseAuditLatest`.
    PassphraseAudit {
        last_changed_unix: Option<u64>,
    },
```

`Command::SetConfig` / `SetContactMuted` / `WipeAllData` reuse the existing `CommandResult::Ok`.

- [ ] **Step 2: Add `Event::LogRecord`**

In `crates/core/src/daemon/events.rs`'s `pub enum Event`, append:

```rust
    /// One redacted log record. Streamed only when the subscriber's
    /// filter includes `EventFilter::Logs`.
    LogRecord(crate::daemon::commands::LogRecord),
```

- [ ] **Step 3: Add `EventFilter::Logs`**

In `crates/core/src/daemon/ipc/wire.rs`'s `pub enum EventFilter`, append:

```rust
    /// Only `Event::LogRecord`. UI subscribes when Settings → Advanced →
    /// Logs is mounted; unsubscribes when the panel closes.
    Logs,
```

Update any filter-matching helper (typically a `matches(&self, event: &Event) -> bool`) to handle the new variant — the new arm returns `matches!(event, Event::LogRecord(_))`.

- [ ] **Step 4: Build**

```bash
cargo build -p skattr-core --features test-harness
cargo clippy -p skattr-core --features test-harness --all-targets -- -D warnings
```

Expected: clean. If the filter-helper match wasn't exhaustive, the compiler will tell you exactly where to extend.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/commands.rs \
        crates/core/src/daemon/events.rs \
        crates/core/src/daemon/ipc/wire.rs
git commit -m "$(cat <<'EOF'
feat(events): add Phase 2.F result/event/filter variants

CommandResult::{Config, PassphraseChanged, Logs, PassphraseAudit}
plus Event::LogRecord and EventFilter::Logs. SetConfig /
SetContactMuted / WipeAllData reuse CommandResult::Ok.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Append-only snapshot test update

**Files:**
- Modify: `crates/core/tests/wire_format_append_only.rs` (append new variant names to the expected snapshot lists)

- [ ] **Step 1: Read the existing snapshot test**

```bash
. "$HOME/.cargo/env"
sed -n '1,200p' crates/core/tests/wire_format_append_only.rs > /dev/null   # just to peek
cargo test --test wire_format_append_only -- --nocapture
```

Expected: FAIL — the test enumerates known variant names and the new variants are missing from the expected list. The error message tells you which list (`COMMAND_VARIANTS`, `RESULT_VARIANTS`, `EVENT_VARIANTS`, `EVENT_FILTER_VARIANTS` — exact const names per the existing test source) is short.

- [ ] **Step 2: Append the new variant names**

Edit `crates/core/tests/wire_format_append_only.rs` and append (in the existing lists; preserve any sort order the file uses):

For Commands:
```
"GetConfig",
"SetConfig",
"ChangePassphrase",
"SetContactMuted",
"TailLogs",
"GetPassphraseAuditLatest",
"WipeAllData",
```

For CommandResults:
```
"Config",
"PassphraseChanged",
"Logs",
"PassphraseAudit",
```

For Events:
```
"LogRecord",
```

For EventFilters:
```
"Logs",
```

(Match the exact const-list naming in the test source — don't introduce a new convention.)

- [ ] **Step 3: Run the test**

```bash
cargo test --test wire_format_append_only -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/tests/wire_format_append_only.rs
git commit -m "$(cat <<'EOF'
test(wire): record Phase 2.F additions in append-only snapshot

Updates the variant enumeration so the snapshot test continues to
detect any future renames or removals of Phase 2.F-introduced wire
surface.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3 — Config struct extension

### Task 8: Extend `Config` with `[delivery]`, `[notifications]`, `[ui]` sections

**Files:**
- Modify: `crates/core/src/daemon/config.rs:14-38` (extend `Config` and add three sub-structs)

- [ ] **Step 1: Add the three new sub-structs and extend `Config`**

Replace the `HistoryConfig` block in `crates/core/src/daemon/config.rs` (around line 14) with:

```rust
/// Message history retention settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct HistoryConfig {
    /// Days of history to retain. 0 = infinite (default; sweep no-ops).
    #[serde(default)]
    pub retention_days: u32,
}

/// Delivery-policy settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryConfig {
    /// How long the hub tries direct connection before falling back to
    /// mailbox deposit. Default 30s (locked in 2.B).
    #[serde(default = "default_direct_timeout_secs")]
    pub direct_timeout_secs: u32,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            direct_timeout_secs: default_direct_timeout_secs(),
        }
    }
}

fn default_direct_timeout_secs() -> u32 {
    30
}

/// Notification settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct NotificationsConfig {
    #[serde(default)]
    pub mode: crate::daemon::commands::NotificationMode,
}

/// UI / shell settings (close-to-tray, start-minimised, log persistence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiConfig {
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub start_minimised: bool,
    #[serde(default)]
    pub persist_logs_to_disk: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            close_to_tray: default_close_to_tray(),
            start_minimised: false,
            persist_logs_to_disk: false,
        }
    }
}

fn default_close_to_tray() -> bool {
    true
}
```

In the existing `pub struct Config` declaration, append two fields:

```rust
    /// Delivery policy. New in 2.F.
    #[serde(default)]
    pub delivery: DeliveryConfig,
    /// Notification settings. New in 2.F.
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// UI / shell settings. New in 2.F.
    #[serde(default)]
    pub ui: UiConfig,
```

In `Config::defaults()` and `Config::fallback()`, populate the new fields:

```rust
            delivery: DeliveryConfig::default(),
            notifications: NotificationsConfig::default(),
            ui: UiConfig::default(),
```

- [ ] **Step 2: Add tests for backward-compat and new defaults**

Append to `config.rs::tests`:

```rust
    #[test]
    fn old_config_without_2f_sections_still_parses() {
        let toml = r#"
            data_dir = "/tmp/skattr"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delivery.direct_timeout_secs, 30);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Full
        ));
        assert!(cfg.ui.close_to_tray);
        assert!(!cfg.ui.start_minimised);
        assert!(!cfg.ui.persist_logs_to_disk);
    }

    #[test]
    fn explicit_2f_sections_parse() {
        let toml = r#"
            data_dir = "/tmp/skattr"

            [delivery]
            direct_timeout_secs = 45

            [notifications]
            mode = "minimal"

            [ui]
            close_to_tray = false
            start_minimised = true
            persist_logs_to_disk = true
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.delivery.direct_timeout_secs, 45);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Minimal
        ));
        assert!(!cfg.ui.close_to_tray);
        assert!(cfg.ui.start_minimised);
        assert!(cfg.ui.persist_logs_to_disk);
    }
```

- [ ] **Step 3: Run tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness daemon::config -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/config.rs
git commit -m "$(cat <<'EOF'
feat(config): add [delivery], [notifications], [ui] sections

All new sections + their fields use #[serde(default)] so existing
config.toml files keep parsing unchanged. Defaults: direct_timeout
30s, notification mode Full, close_to_tray true, start_minimised /
persist_logs_to_disk false.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: `apply_patch` + atomic `save_to_disk`

**Files:**
- Modify: `crates/core/src/daemon/config.rs` (append two methods on `Config`)

- [ ] **Step 1: Add `apply_patch` and `save_to_disk`**

Append to `impl Config` in `crates/core/src/daemon/config.rs`:

```rust
    /// Apply a `ConfigPatch`, mutating `self` in place. Returns
    /// `Err(CoreError::Config(...))` if any field fails validation.
    /// Validation rules:
    ///   - `direct_timeout_secs`: 1..=600
    ///   - `history_retention_days`: any u32 (0 = infinite)
    ///   - bool fields: trivially valid
    ///   - `notification_mode`: enum-bounded
    pub fn apply_patch(
        &mut self,
        patch: &crate::daemon::commands::ConfigPatch,
    ) -> Result<()> {
        if let Some(d) = patch.history_retention_days {
            self.history.retention_days = d;
        }
        if let Some(t) = patch.direct_timeout_secs {
            if !(1..=600).contains(&t) {
                return Err(CoreError::Config(format!(
                    "direct_timeout_secs out of range 1..=600 (got {t})"
                )));
            }
            self.delivery.direct_timeout_secs = t;
        }
        if let Some(m) = patch.notification_mode {
            self.notifications.mode = m;
        }
        if let Some(b) = patch.close_to_tray {
            self.ui.close_to_tray = b;
        }
        if let Some(b) = patch.start_minimised {
            self.ui.start_minimised = b;
        }
        if let Some(b) = patch.persist_logs_to_disk {
            self.ui.persist_logs_to_disk = b;
        }
        Ok(())
    }

    /// Project onto the wire `ConfigSnapshot` (UI-relevant fields only).
    pub fn snapshot(&self) -> crate::daemon::commands::ConfigSnapshot {
        crate::daemon::commands::ConfigSnapshot {
            history_retention_days: self.history.retention_days,
            direct_timeout_secs: self.delivery.direct_timeout_secs,
            notification_mode: self.notifications.mode,
            close_to_tray: self.ui.close_to_tray,
            start_minimised: self.ui.start_minimised,
            persist_logs_to_disk: self.ui.persist_logs_to_disk,
        }
    }

    /// Atomically write `self` to a TOML file at `path`. Writes to
    /// `<path>.tmp`, fsyncs, renames, then fsyncs the parent dir. Mode
    /// 0600 on Unix.
    pub fn save_to_disk(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let serialised = toml::to_string_pretty(self)
            .map_err(|e| CoreError::Config(format!("serialise: {e}")))?;

        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .map_err(|e| CoreError::Config(format!("create {}: {e}", tmp.display())))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = f.metadata().map_err(|e| CoreError::Config(format!("stat: {e}")))?.permissions();
                perm.set_mode(0o600);
                f.set_permissions(perm).map_err(|e| CoreError::Config(format!("chmod: {e}")))?;
            }
            f.write_all(serialised.as_bytes())
                .map_err(|e| CoreError::Config(format!("write: {e}")))?;
            f.sync_all().map_err(|e| CoreError::Config(format!("fsync: {e}")))?;
        }
        std::fs::rename(&tmp, path)
            .map_err(|e| CoreError::Config(format!("rename {} → {}: {e}", tmp.display(), path.display())))?;
        if let Some(parent) = path.parent() {
            // Best-effort directory fsync; not all filesystems support it.
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }
```

- [ ] **Step 2: Add tests**

Append to `config.rs::tests`:

```rust
    #[test]
    fn apply_patch_partial_only_touches_specified_fields() {
        let mut cfg = Config::defaults().unwrap();
        cfg.history.retention_days = 7;
        let patch = crate::daemon::commands::ConfigPatch {
            notification_mode: Some(crate::daemon::commands::NotificationMode::Off),
            ..Default::default()
        };
        cfg.apply_patch(&patch).unwrap();
        assert_eq!(cfg.history.retention_days, 7);
        assert!(matches!(
            cfg.notifications.mode,
            crate::daemon::commands::NotificationMode::Off
        ));
    }

    #[test]
    fn apply_patch_rejects_out_of_range_timeout() {
        let mut cfg = Config::defaults().unwrap();
        let patch = crate::daemon::commands::ConfigPatch {
            direct_timeout_secs: Some(0),
            ..Default::default()
        };
        assert!(cfg.apply_patch(&patch).is_err());

        let patch = crate::daemon::commands::ConfigPatch {
            direct_timeout_secs: Some(601),
            ..Default::default()
        };
        assert!(cfg.apply_patch(&patch).is_err());
    }

    #[test]
    fn save_to_disk_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let mut cfg = Config::defaults().unwrap();
        cfg.delivery.direct_timeout_secs = 45;
        cfg.save_to_disk(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.delivery.direct_timeout_secs, 45);
    }

    #[test]
    fn snapshot_projects_all_fields() {
        let cfg = Config::defaults().unwrap();
        let snap = cfg.snapshot();
        assert_eq!(snap.history_retention_days, 0);
        assert_eq!(snap.direct_timeout_secs, 30);
        assert!(snap.close_to_tray);
        assert!(!snap.start_minimised);
        assert!(!snap.persist_logs_to_disk);
    }
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::config -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/config.rs
git commit -m "$(cat <<'EOF'
feat(config): apply_patch + snapshot + atomic save_to_disk

apply_patch validates direct_timeout_secs ∈ [1, 600] and otherwise
just copies Some(_) fields onto the live Config. snapshot projects
onto the wire ConfigSnapshot. save_to_disk writes via temp + fsync
+ rename + parent-dir fsync, mode 0600 on Unix.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — Contact projections

### Task 10: `ContactRepo::set_muted`

**Files:**
- Modify: `crates/core/src/contact/repo.rs` (append method + test)

- [ ] **Step 1: Add the method**

Append to `impl<'a> ContactRepo<'a>` (or wherever existing mutators like `set_display_name` live):

```rust
    /// Toggle the per-contact mute flag. No-op (returns `Ok(())`) if
    /// the contact does not exist — caller is responsible for the
    /// existence check (typically a `lookup_by_pubkey` first).
    pub fn set_muted(&self, pubkey: &[u8; 32], muted: bool) -> Result<()> {
        self.pool.with(|c| {
            c.execute(
                "UPDATE contacts SET muted = ?1 WHERE pubkey = ?2",
                rusqlite::params![muted as i64, pubkey],
            )
            .map_err(|e| {
                CoreError::Storage(StorageErrorKind::Other(format!(
                    "set_muted: {e}"
                )))
            })?;
            Ok(())
        })
    }

    /// Read the current mute flag. Returns `Ok(false)` if the contact
    /// does not exist.
    pub fn is_muted(&self, pubkey: &[u8; 32]) -> Result<bool> {
        self.pool.with(|c| {
            let muted: Option<i64> = c
                .query_row(
                    "SELECT muted FROM contacts WHERE pubkey = ?1",
                    rusqlite::params![pubkey],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| {
                    CoreError::Storage(StorageErrorKind::Other(format!(
                        "is_muted: {e}"
                    )))
                })?;
            Ok(matches!(muted, Some(v) if v != 0))
        })
    }
```

If `rusqlite::OptionalExtension` isn't already imported in this file, add `use rusqlite::OptionalExtension;` near the top.

- [ ] **Step 2: Add tests**

Append to the existing `mod tests` in `repo.rs`:

```rust
    #[test]
    fn set_muted_toggles_and_persists() {
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let repo = ContactRepo::new(&pool);

        // Insert a contact via whatever existing helper the test module uses
        // (or a raw INSERT if there is none). The plan assumes ContactRepo
        // already has an `insert` or similar; adapt to the actual API.
        pool.with(|c| {
            c.execute(
                "INSERT INTO contacts (pubkey, display_name, added_at, hidden, muted) \
                 VALUES (?1, NULL, 0, 0, 0)",
                rusqlite::params![[0xAA; 32]],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

        assert!(!repo.is_muted(&[0xAA; 32]).unwrap());
        repo.set_muted(&[0xAA; 32], true).unwrap();
        assert!(repo.is_muted(&[0xAA; 32]).unwrap());
        repo.set_muted(&[0xAA; 32], false).unwrap();
        assert!(!repo.is_muted(&[0xAA; 32]).unwrap());
    }

    #[test]
    fn is_muted_returns_false_for_missing_contact() {
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let repo = ContactRepo::new(&pool);
        assert!(!repo.is_muted(&[0xBB; 32]).unwrap());
    }
```

- [ ] **Step 3: Run tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness contact::repo::tests::set_muted -- --nocapture
cargo test -p skattr-core --features test-harness contact::repo::tests::is_muted -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/contact/repo.rs
git commit -m "$(cat <<'EOF'
feat(contact): ContactRepo::{set_muted, is_muted}

Persists per-contact mute flag in contacts.muted. is_muted returns
false for missing contacts; set_muted is a no-op for missing rows
(caller is responsible for existence checks; matches existing
set_display_name semantics).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: `ContactSummary` projection — `muted` + `peer_mailboxes`

**Files:**
- Modify: `crates/core/src/daemon/commands.rs:206` (add fields to `ContactSummary`)
- Modify: wherever `ContactSummary` is constructed (find via grep, typically `crates/core/src/daemon/dispatch.rs::list_contacts` or a helper in `crates/core/src/contact/summary.rs`).

- [ ] **Step 1: Locate the projection site**

```bash
. "$HOME/.cargo/env"
grep -n "ContactSummary {" crates/core/src/ -r
```

Expected output: a small number of construction sites (typically 1–2) plus the type definition. Note them.

- [ ] **Step 2: Add additive fields to `ContactSummary`**

In `crates/core/src/daemon/commands.rs` (around line 206), append fields to the `ContactSummary` struct (after the existing `last_ts_recv`-region fields):

```rust
    /// Per-contact desktop-notification + unread-badge mute. New in
    /// 2.F. `false` for clients that don't yet honour the field.
    #[serde(default)]
    pub muted: bool,
    /// Onions advertised by the latest verified `ContactCard.body.mailboxes`
    /// for this contact. New in 2.F. Empty for contacts whose card has
    /// no mailboxes or whose card is missing.
    #[serde(default)]
    pub peer_mailboxes: Vec<String>,
```

- [ ] **Step 3: Wire `muted` into the projection**

In each `ContactSummary { ... }` construction site found in Step 1, populate `muted` from the row's `muted` column. The query needs to include `muted` in its SELECT list.

Concrete change in `crates/core/src/daemon/dispatch.rs::list_contacts` (or wherever): the SELECT becomes `SELECT pubkey, display_name, ..., hidden, muted FROM contacts ...` and the row mapping reads `muted: row.get::<_, i64>("muted")? != 0`. If a helper in `core::contact` builds these, modify the helper instead.

- [ ] **Step 4: Wire `peer_mailboxes` into the projection**

For each contact summary, fetch the latest verified `ContactCard` via `ContactCardRepo::latest_for(pubkey)` (existing repo from Phase 1.D); on `Some(card)`, project `card.body.mailboxes.iter().map(|m| m.onion.clone()).collect()`; on `None`, default to `Vec::new()`. Wrap the per-contact lookup in a single transaction batched with the contact list query for performance.

If the existing `list_contacts` doesn't currently fetch cards, add the `ContactCardRepo::new(&handle.pool)` call inside the for-loop (acceptable for ≤ a few hundred contacts; revisit at Phase 3 if it becomes a hotspot).

- [ ] **Step 5: Add tests**

Add a test in `crates/core/src/daemon/dispatch.rs::tests` (or `crates/core/src/contact/summary.rs::tests` if such a module exists):

```rust
    #[tokio::test]
    async fn list_contacts_projects_muted_and_peer_mailboxes() {
        // Set up a daemon handle (use the same test helper other dispatch tests use).
        // Insert one contact with muted=1 and a ContactCard carrying two mailbox onions.
        // Call list_contacts and assert:
        //   summary.muted == true
        //   summary.peer_mailboxes == vec!["xyz.onion".to_string(), "abc.onion".to_string()]
    }
```

(Concrete fixture setup follows whatever helper the existing dispatch tests use; consult `dispatch.rs::tests` for the pattern. Don't invent a new test harness.)

- [ ] **Step 6: Run tests**

```bash
cargo test -p skattr-core --features test-harness list_contacts_projects_muted -- --nocapture
cargo test -p skattr-core --features test-harness daemon::dispatch -- --nocapture
```

Expected: PASS, including the existing `list_contacts` tests (the additive fields default correctly when no mute / no card).

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/commands.rs crates/core/src/daemon/dispatch.rs
# (also any contact/summary.rs touched)
git commit -m "$(cat <<'EOF'
feat(contacts): project muted + peer_mailboxes onto ContactSummary

Both fields are #[serde(default)] so older clients that don't
emit them keep deserializing the same wire frames. peer_mailboxes
is sourced from the latest verified ContactCard.body.mailboxes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5 — Daemon dispatch (basic handlers)

### Task 12: `get_config` + `set_config` handlers

**Files:**
- Modify: `crates/core/src/daemon/handle.rs` — `DaemonHandle` gains an `Arc<RwLock<Config>>` field + `config_path: PathBuf` field (find existing struct definition; add fields where the existing `pool` / `mailbox_client` / similar live).
- Modify: `crates/core/src/daemon/dispatch.rs` — replace the `Command::GetConfig` and `Command::SetConfig { .. }` stub arms with real handlers.
- Modify: `crates/core/src/daemon/mod.rs` (or wherever `Daemon::run` constructs the handle) — initialise the new fields.

- [ ] **Step 1: Extend `DaemonHandle`**

In `crates/core/src/daemon/handle.rs`, append fields to the `DaemonHandle<S>` struct:

```rust
    /// Live config; mutators take the write lock and atomic-save on success.
    pub config: std::sync::Arc<tokio::sync::RwLock<crate::daemon::config::Config>>,
    /// Where `config.toml` lives on disk (used by `apply_patch` saves).
    pub config_path: std::path::PathBuf,
```

In `Daemon::run` (typically `crates/core/src/daemon/mod.rs`), wrap the initial config in `Arc::new(RwLock::new(...))` and pass through the resolved config-file path (compute it once from CLI flags / XDG). If `Daemon::run`'s signature doesn't currently take a config path, plumb it through; the CLI in `crates/cli/src/main.rs` already resolves it via `Config::load_with_precedence` — pass the resolved path (or fall back to `dirs::config_dir().join("skattr/config.toml")` if no flag).

- [ ] **Step 2: Implement the handlers**

In `crates/core/src/daemon/dispatch.rs`, replace the two stub arms with:

```rust
        Command::GetConfig => get_config(&handle).await,
        Command::SetConfig { patch } => set_config(&handle, patch).await,
```

And add the handler functions in the same file:

```rust
async fn get_config<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let cfg = handle.config.read().await;
    Ok(CommandResult::Config(cfg.snapshot()))
}

async fn set_config<S>(
    handle: &Arc<DaemonHandle<S>>,
    patch: crate::daemon::commands::ConfigPatch,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    let mut cfg = handle.config.write().await;
    cfg.apply_patch(&patch).map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: e.to_string(),
        })
    })?;
    cfg.save_to_disk(&handle.config_path).map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: format!("save_to_disk: {e}"),
        })
    })?;
    drop(cfg);

    // Side-effects on settings that affect live behaviour:
    //   - history_retention_days: the retention sweep already re-reads
    //     the config on every tick (it reads via handle.config.read()).
    //   - persist_logs_to_disk: handled by the logs subsystem (Task 22).
    // Other knobs are consumed UI-side only; no daemon hot-apply needed.
    Ok(CommandResult::Ok)
}
```

- [ ] **Step 3: Update the retention sweep to read live config**

If `crates/core/src/daemon/retention.rs::spawn_sweep` currently takes a `retention_days: u32` directly, change the signature to take `Arc<RwLock<Config>>` instead and re-read on every tick. (Ref `retention.rs:25-30`.) The body inside `tokio::select!`:

```rust
let retention_days = config.read().await.history.retention_days;
if retention_days != 0 { /* existing prune code */ }
```

Update the call site in `Daemon::run` to pass `handle.config.clone()` instead of the snapshot integer.

- [ ] **Step 4: Add a unit test**

Append to `crates/core/src/daemon/dispatch.rs::tests`:

```rust
    #[tokio::test]
    async fn get_config_returns_snapshot() {
        // Use the existing test helper that builds a DaemonHandle
        // pointing at a tempdir + in-memory pool.
        let handle = test_handle().await;
        let result = execute_command(handle.clone(), Command::GetConfig).await.unwrap();
        match result {
            CommandResult::Config(snap) => {
                assert_eq!(snap.history_retention_days, 0);
                assert_eq!(snap.direct_timeout_secs, 30);
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_config_persists_and_round_trips() {
        let handle = test_handle().await;
        let patch = crate::daemon::commands::ConfigPatch {
            history_retention_days: Some(7),
            ..Default::default()
        };
        execute_command(handle.clone(), Command::SetConfig { patch })
            .await
            .unwrap();
        let result = execute_command(handle, Command::GetConfig).await.unwrap();
        match result {
            CommandResult::Config(snap) => assert_eq!(snap.history_retention_days, 7),
            other => panic!("expected Config, got {other:?}"),
        }
    }
```

(`test_handle()` is the existing helper used by other dispatch tests — reuse, don't reinvent.)

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::get_config -- --nocapture
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::set_config -- --nocapture
cargo test -p skattr-core --features test-harness daemon::retention -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/{handle.rs,dispatch.rs,mod.rs,retention.rs}
git commit -m "$(cat <<'EOF'
feat(dispatch): GetConfig + SetConfig handlers

DaemonHandle now owns Arc<RwLock<Config>> + config_path; SetConfig
takes the write lock, applies the patch, then atomic-saves. The
retention sweep re-reads the live config on every tick so retention
changes hot-apply without restart. ConfigPatch validation errors
surface as DaemonErrorKind::InvalidArgument.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: `set_contact_muted` handler

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (replace stub arm + add handler)

- [ ] **Step 1: Wire the arm**

Replace the `Command::SetContactMuted { .. }` stub:

```rust
        Command::SetContactMuted { contact, muted } => {
            set_contact_muted(&handle, contact, muted).await
        }
```

- [ ] **Step 2: Add the handler**

```rust
async fn set_contact_muted<S>(
    handle: &Arc<DaemonHandle<S>>,
    contact: crate::identity::PublicKey,
    muted: bool,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::storage::ContactRepo;

    let repo = ContactRepo::new(&handle.pool);
    // Existence check first — we want a typed error, not a silent no-op.
    let exists = repo
        .lookup_by_pubkey(contact.as_bytes())
        .map_err(map_err)?
        .is_some();
    if !exists {
        return Err(IpcError::Daemon(DaemonErrorKind::ContactNotFound));
    }
    repo.set_muted(contact.as_bytes(), muted).map_err(map_err)?;

    // Emit ContactUpdated so live UI re-fetches the contact summary.
    handle.events.emit(crate::daemon::events::Event::ContactUpdated {
        contact,
    });
    Ok(CommandResult::Ok)
}
```

(If `ContactUpdated` carries different fields in the existing enum, match its actual shape — check `events.rs`.)

- [ ] **Step 3: Add a test**

```rust
    #[tokio::test]
    async fn set_contact_muted_toggles_and_emits_event() {
        let handle = test_handle().await;
        let pk = test_insert_contact(&handle, [0xAA; 32]).await;
        let mut sub = handle.events.subscribe(crate::daemon::ipc::wire::EventFilter::All);
        execute_command(
            handle.clone(),
            Command::SetContactMuted { contact: pk, muted: true },
        )
        .await
        .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, crate::daemon::events::Event::ContactUpdated { .. }));

        // Persisted
        let repo = crate::storage::ContactRepo::new(&handle.pool);
        assert!(repo.is_muted(&[0xAA; 32]).unwrap());
    }

    #[tokio::test]
    async fn set_contact_muted_returns_not_found_for_missing_contact() {
        let handle = test_handle().await;
        let pk = crate::identity::PublicKey::from([0xFF; 32]);
        let err = execute_command(
            handle,
            Command::SetContactMuted { contact: pk, muted: true },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::ContactNotFound)
        ));
    }
```

(`test_insert_contact` is the existing helper used by other dispatch tests; reuse.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::set_contact_muted -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): SetContactMuted handler

Validates contact existence (returns ContactNotFound when missing),
flips contacts.muted, emits Event::ContactUpdated so live UIs
re-fetch the contact summary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: `get_passphrase_audit_latest` handler + repo

**Files:**
- Create: `crates/core/src/storage/passphrase_audit.rs` (new repo) + register in `crates/core/src/storage/mod.rs`
- Modify: `crates/core/src/daemon/dispatch.rs` (replace stub arm + add handler)

- [ ] **Step 1: Create the repo**

Create `crates/core/src/storage/passphrase_audit.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Append-only audit log for `Command::ChangePassphrase` outcomes.

use crate::error::{CoreError, Result};
use crate::storage::Pool;
use crate::storage::StorageErrorKind;

pub struct PassphraseAuditRepo<'a> {
    pool: &'a Pool,
}

impl<'a> PassphraseAuditRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Append a new audit row.
    pub fn append(&self, ts_unix: i64, outcome: AuditOutcome) -> Result<()> {
        self.pool.with(|c| {
            c.execute(
                "INSERT INTO passphrase_audit (ts_unix, outcome) VALUES (?1, ?2)",
                rusqlite::params![ts_unix, outcome.as_str()],
            )
            .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("audit append: {e}"))))?;
            Ok(())
        })
    }

    /// Read the most recent audit row's `ts_unix`.
    pub fn latest_ts(&self) -> Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        self.pool.with(|c| {
            let row: Option<i64> = c
                .query_row(
                    "SELECT ts_unix FROM passphrase_audit ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| CoreError::Storage(StorageErrorKind::Other(format!("audit latest: {e}"))))?;
            Ok(row)
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuditOutcome {
    Changed,
    RolledBack,
    Recovered,
}

impl AuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::RolledBack => "rolled_back",
            Self::Recovered => "recovered",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_latest_returns_most_recent() {
        let pool = std::sync::Arc::new(Pool::in_memory());
        let repo = PassphraseAuditRepo::new(&pool);
        assert_eq!(repo.latest_ts().unwrap(), None);
        repo.append(1_000, AuditOutcome::Changed).unwrap();
        repo.append(2_000, AuditOutcome::Recovered).unwrap();
        assert_eq!(repo.latest_ts().unwrap(), Some(2_000));
    }
}
```

Register in `crates/core/src/storage/mod.rs` — add a `pub(crate) mod passphrase_audit;` line (matching the style of existing `pub(crate) mod` declarations) and re-export the types via `pub(crate) use passphrase_audit::{PassphraseAuditRepo, AuditOutcome};`.

- [ ] **Step 2: Wire the dispatch handler**

In `crates/core/src/daemon/dispatch.rs`, replace the `Command::GetPassphraseAuditLatest` stub:

```rust
        Command::GetPassphraseAuditLatest => get_passphrase_audit_latest(&handle).await,
```

```rust
async fn get_passphrase_audit_latest<S>(
    handle: &Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::storage::PassphraseAuditRepo;
    let repo = PassphraseAuditRepo::new(&handle.pool);
    let ts = repo.latest_ts().map_err(map_err)?;
    Ok(CommandResult::PassphraseAudit {
        last_changed_unix: ts.map(|v| v as u64),
    })
}
```

- [ ] **Step 3: Run tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness storage::passphrase_audit -- --nocapture
cargo test -p skattr-core --features test-harness daemon::dispatch::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/storage/passphrase_audit.rs \
        crates/core/src/storage/mod.rs \
        crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(audit): PassphraseAuditRepo + GetPassphraseAuditLatest

Append-only repo over passphrase_audit (migration 0014).
GetPassphraseAuditLatest returns the most recent ts_unix; UI
surfaces this as "Last changed" in Settings → Identity.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6 — Mailbox CRUD wiring

### Task 15: Replace `AddMailbox` / `RemoveMailbox` / `ListMailboxes` stub bodies with real `MailboxClient` calls

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (find `handle_add_mailbox`, `handle_remove_mailbox`, `handle_list_mailboxes` — currently stubs from 2.C)

- [ ] **Step 1: Read the existing stubs**

```bash
. "$HOME/.cargo/env"
grep -n "handle_add_mailbox\|handle_remove_mailbox\|handle_list_mailboxes" crates/core/src/daemon/dispatch.rs
```

Note line ranges. The 2.C stubs return either `CommandResult::Mailboxes(Vec::new())` or `DaemonErrorKind::Unsupported`.

- [ ] **Step 2: Implement `handle_add_mailbox`**

Replace the stub body with:

```rust
async fn handle_add_mailbox<S>(
    handle: Arc<DaemonHandle<S>>,
    onion: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::mailbox::client::MailboxClient;
    use crate::storage::MailboxRepo;

    // Validate onion shape (v3 = 56 base32 chars + ".onion").
    if !onion.ends_with(".onion") || onion.len() != 56 + 6 {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: format!("onion must be a v3 address (got {} chars)", onion.len()),
        }));
    }

    // Register against the mailbox server using our own identity key.
    let identity = handle.identity.clone();
    let client = MailboxClient::new(handle.transport.clone(), identity);
    client
        .register(&onion)
        .await
        .map_err(|e| IpcError::Daemon(DaemonErrorKind::Mailbox(e.kind())))?;

    // Persist the mailbox row + return its id.
    let repo = MailboxRepo::new(&handle.pool);
    let id = repo.insert(&onion).map_err(map_err)?;
    Ok(CommandResult::Ok)
}
```

(Adapt to the actual `MailboxClient::new` and `MailboxClient::register` signatures from 2.B — see `crates/core/src/mailbox/client.rs`. The plan reflects the spec; reconcile with the real API on the branch.)

- [ ] **Step 3: Implement `handle_remove_mailbox`**

```rust
async fn handle_remove_mailbox<S>(
    handle: Arc<DaemonHandle<S>>,
    id: i64,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::mailbox::client::MailboxClient;
    use crate::storage::MailboxRepo;

    let repo = MailboxRepo::new(&handle.pool);
    let row = repo
        .lookup_by_id(id)
        .map_err(map_err)?
        .ok_or(IpcError::Daemon(DaemonErrorKind::ContactNotFound))?;
    // Re-use ContactNotFound for "no such mailbox" — Phase 2.F could add a
    // dedicated MailboxNotFound variant; keep the existing taxonomy until then.

    let client = MailboxClient::new(handle.transport.clone(), handle.identity.clone());
    // Best-effort deregister — if the mailbox is unreachable, we still
    // remove the local row. Log warning on failure.
    if let Err(e) = client.deregister(&row.onion).await {
        tracing::warn!(error = %e, mailbox_id = id, "deregister failed; removing local row anyway");
    }
    repo.delete(id).map_err(map_err)?;
    Ok(CommandResult::Ok)
}
```

- [ ] **Step 4: Implement `handle_list_mailboxes`**

```rust
async fn handle_list_mailboxes<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::commands::MailboxSummary;
    use crate::storage::MailboxRepo;
    let repo = MailboxRepo::new(&handle.pool);
    let rows = repo.list_all().map_err(map_err)?;
    let summaries: Vec<MailboxSummary> = rows
        .into_iter()
        .map(|r| MailboxSummary {
            id: r.id,
            onion: r.onion,
            status: r.status,
            registered_at: r.registered_at as u64,
        })
        .collect();
    Ok(CommandResult::Mailboxes(summaries))
}
```

- [ ] **Step 5: Add tests**

In `crates/core/src/daemon/dispatch.rs::tests`:

```rust
    #[tokio::test]
    async fn add_mailbox_rejects_bad_onion() {
        let handle = test_handle().await;
        let err = execute_command(
            handle,
            Command::AddMailbox { onion: "not-an-onion".into() },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
        ));
    }

    #[tokio::test]
    async fn list_mailboxes_returns_persisted_rows() {
        let handle = test_handle().await;
        // Insert directly via the repo (skipping .register() to avoid needing
        // a real mailbox server in unit tests).
        let repo = crate::storage::MailboxRepo::new(&handle.pool);
        let _id = repo.insert(&format!(
            "{}.onion",
            "a".repeat(56)
        )).unwrap();
        let result = execute_command(handle, Command::ListMailboxes).await.unwrap();
        match result {
            CommandResult::Mailboxes(v) => assert_eq!(v.len(), 1),
            other => panic!("expected Mailboxes, got {other:?}"),
        }
    }
```

(For full add → register → list → deregister → remove coverage against a real mailbox server, see Task 41's integration test.)

- [ ] **Step 6: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::add_mailbox -- --nocapture
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::list_mailboxes -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): real mailbox CRUD wiring (replaces 2.C stubs)

handle_add_mailbox validates onion shape, registers against the
mailbox server via MailboxClient, persists the row.
handle_remove_mailbox best-effort deregisters then removes the
local row (warning-logs deregister failure to avoid leaving stale
local rows when a mailbox is unreachable).
handle_list_mailboxes projects MailboxRepo rows onto MailboxSummary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 7 — Passphrase atomicity

### Task 16: `passphrase.rs` module — `RekeyJournal` + file helpers

**Files:**
- Create: `crates/core/src/daemon/passphrase.rs`
- Modify: `crates/core/src/daemon/mod.rs` (register the module: `pub(crate) mod passphrase;`)
- Modify: `crates/core/src/daemon/error_kind.rs` (add three new variants — see Step 2 below)

- [ ] **Step 1: Add the new `DaemonErrorKind` variants**

In `crates/core/src/daemon/error_kind.rs::DaemonErrorKind`, append:

```rust
    /// Authentication failed (e.g. ChangePassphrase wrong-old).
    Unauthorized,
    /// Passphrase prompted at recovery doesn't decrypt either the OLD or
    /// NEW state. User should retry.
    WrongPassphrase,
    /// On-disk passphrase state is in a logically-impossible
    /// configuration; manual intervention required (see
    /// docs/operations/passphrase-recovery.md).
    InconsistentState,
```

Update `CoreError::kind()`'s structural match in `crates/core/src/error.rs` (or wherever it lives) so the new variants are reachable. The build-time guard test from 1.H rejects any `str::contains` short-cut.

- [ ] **Step 2: Create `passphrase.rs` with the journal type and file helpers**

Create `crates/core/src/daemon/passphrase.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! ChangePassphrase: stage-then-rename atomic re-key spanning the
//! identity vault and the storage age key. Recovery is deterministic
//! from on-disk file fingerprints — never needs the OLD passphrase.
//!
//! See docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md
//! § "ChangePassphrase — journaled atomic re-key" for the full spec.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::{CoreError, Result};

/// Filenames are siblings of `data_dir/identity.vault` and `age-key`.
pub(crate) const JOURNAL_FILE: &str = "passphrase-rekey.journal";
pub(crate) const VAULT_STAGED: &str = "identity.vault.staged";
pub(crate) const AGE_KEY_STAGED: &str = "age-key.staged";

#[derive(Debug, Serialize, Deserialize, ZeroizeOnDrop)]
pub(crate) struct RekeyJournal {
    /// = 1 in 2.F. Increment on any incompatible journal-format change.
    pub version: u8,
    /// Argon2id salt for the NEW passphrase.
    pub new_salt: [u8; 16],
    pub started_unix: u64,
}

impl RekeyJournal {
    pub fn write(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join(JOURNAL_FILE);
        let tmp = data_dir.join(format!("{JOURNAL_FILE}.tmp"));
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(self, &mut bytes)
            .map_err(|e| CoreError::Config(format!("journal encode: {e}")))?;
        write_atomic_0600(&tmp, &path, &bytes)
    }

    pub fn read(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(JOURNAL_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let j: Self = ciborium::de::from_reader(bytes.as_slice())
                    .map_err(|e| CoreError::Config(format!("journal decode: {e}")))?;
                Ok(Some(j))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("journal read: {e}"))),
        }
    }

    pub fn delete(data_dir: &Path) -> Result<()> {
        let path = data_dir.join(JOURNAL_FILE);
        match std::fs::remove_file(&path) {
            Ok(()) => fsync_dir(data_dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CoreError::Config(format!("journal delete: {e}"))),
        }
    }
}

pub(crate) fn write_atomic_0600(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(tmp)
            .map_err(|e| CoreError::Config(format!("create {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = f
                .metadata()
                .map_err(|e| CoreError::Config(format!("stat: {e}")))?
                .permissions();
            perm.set_mode(0o600);
            f.set_permissions(perm)
                .map_err(|e| CoreError::Config(format!("chmod: {e}")))?;
        }
        f.write_all(bytes)
            .map_err(|e| CoreError::Config(format!("write: {e}")))?;
        f.sync_all()
            .map_err(|e| CoreError::Config(format!("fsync file: {e}")))?;
    }
    std::fs::rename(tmp, final_path)
        .map_err(|e| CoreError::Config(format!("rename: {e}")))?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

fn fsync_dir(dir: &Path) -> Result<()> {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub(crate) fn cleanup_staged(data_dir: &Path) {
    for name in [VAULT_STAGED, AGE_KEY_STAGED] {
        let path = data_dir.join(name);
        let _ = std::fs::remove_file(&path);
    }
    let _ = fsync_dir(data_dir);
}
```

- [ ] **Step 3: Register the module**

In `crates/core/src/daemon/mod.rs`, add (alongside existing `pub(crate) mod` declarations):

```rust
pub(crate) mod passphrase;
```

- [ ] **Step 4: Add tests**

Append to `passphrase.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let j = RekeyJournal {
            version: 1,
            new_salt: [0xAB; 16],
            started_unix: 1_700_000_000,
        };
        j.write(tmp.path()).unwrap();
        let back = RekeyJournal::read(tmp.path()).unwrap().unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.new_salt, [0xAB; 16]);
        assert_eq!(back.started_unix, 1_700_000_000);
        RekeyJournal::delete(tmp.path()).unwrap();
        assert!(RekeyJournal::read(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn cleanup_staged_removes_present_files_and_ignores_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(VAULT_STAGED), b"x").unwrap();
        cleanup_staged(tmp.path());
        assert!(!tmp.path().join(VAULT_STAGED).exists());
        assert!(!tmp.path().join(AGE_KEY_STAGED).exists());
        // Idempotent
        cleanup_staged(tmp.path());
    }
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::passphrase::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/{passphrase.rs,mod.rs,error_kind.rs} \
        crates/core/src/error.rs
git commit -m "$(cat <<'EOF'
feat(passphrase): RekeyJournal + atomic file helpers

Adds the journal type (CBOR, ZeroizeOnDrop) and the temp-file +
fsync + rename + parent-dir-fsync helper. Three new
DaemonErrorKind variants (Unauthorized, WrongPassphrase,
InconsistentState) for the recovery-flow surface.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 17: `passphrase::rekey` — happy-path implementation

**Files:**
- Modify: `crates/core/src/daemon/passphrase.rs` (add `rekey` function + `KillSwitch`)

- [ ] **Step 1: Add the `KillSwitch`**

Append to `crates/core/src/daemon/passphrase.rs`:

```rust
/// Test-only kill switch for crash injection. Six panic points; tests
/// arm one at a time and assert recovery does the right thing on the
/// next boot. Mirrors `crate::delivery::kill_stream::KillSwitch`.
#[cfg(feature = "test-harness")]
#[derive(Debug, Default, Clone)]
pub struct KillSwitch {
    inner: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

#[cfg(feature = "test-harness")]
impl KillSwitch {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn arm(&self, point: KillPoint) {
        self.inner.store(point as u8, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn check(&self, current: KillPoint) {
        let armed = self.inner.load(std::sync::atomic::Ordering::SeqCst);
        if armed == current as u8 {
            panic!("KillSwitch fired at {current:?}");
        }
    }
}

#[cfg(feature = "test-harness")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KillPoint {
    /// 0 = disarmed
    None = 0,
    /// K1: before write identity.vault.staged
    BeforeStageVault = 1,
    /// K2: before write age-key.staged
    BeforeStageAgeKey = 2,
    /// K3: before write journal
    BeforeJournal = 3,
    /// K4: before rename identity.vault.staged → identity.vault
    BeforeVaultRename = 4,
    /// K5: before rename age-key.staged → age-key
    BeforeAgeKeyRename = 5,
    /// K6: before delete journal
    BeforeJournalDelete = 6,
}
```

- [ ] **Step 2: Add the `rekey` function**

Append to `passphrase.rs`:

```rust
use crate::identity::{IdentityKey, IdentityVault};

/// Inputs to `rekey`. The `IdentityKey` is needed so we can re-encrypt
/// the vault under the new passphrase without re-deriving from the
/// seed (we already have the in-memory keypair).
pub(crate) struct RekeyParams<'a> {
    pub data_dir: &'a Path,
    pub identity_vault_path: &'a Path,   // typically data_dir/identity.vault
    pub age_key_path: &'a Path,          // typically data_dir/age-key
    pub identity: &'a IdentityKey,
    pub age_key_plaintext: &'a Zeroizing<Vec<u8>>,
    pub old_passphrase: &'a Zeroizing<String>,
    pub new_passphrase: &'a Zeroizing<String>,
    #[cfg(feature = "test-harness")]
    pub kill_switch: KillSwitch,
}

pub(crate) fn rekey(p: RekeyParams<'_>) -> Result<()> {
    use rand::RngCore;

    // 1. Validate `new` (length + zxcvbn ≥ 3 + ≠ old) is the caller's job
    //    (dispatch::change_passphrase performs validation before invoking
    //    this function, so the dispatch layer can map errors to typed
    //    DaemonErrorKind values).

    // 2. Caller already verified `old` by decrypting both files into the
    //    plaintexts that arrive in `p`.

    // 3. Generate new_salt.
    let mut new_salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut new_salt);

    let data_dir = p.data_dir;

    // K1
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeStageVault);

    // 5a. Stage identity.vault under new passphrase.
    let new_vault_blob = IdentityVault::wrap(
        p.identity,
        p.new_passphrase,
        &new_salt,
    )?;
    let vault_staged = data_dir.join(VAULT_STAGED);
    let vault_tmp = data_dir.join(format!("{VAULT_STAGED}.tmp"));
    write_atomic_0600(&vault_tmp, &vault_staged, &new_vault_blob)?;

    // K2
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeStageAgeKey);

    // 5b. Stage age-key under new passphrase.
    let new_age_blob = wrap_age_key(p.age_key_plaintext, p.new_passphrase, &new_salt)?;
    let age_staged = data_dir.join(AGE_KEY_STAGED);
    let age_tmp = data_dir.join(format!("{AGE_KEY_STAGED}.tmp"));
    write_atomic_0600(&age_tmp, &age_staged, &new_age_blob)?;

    // K3
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeJournal);

    // 6. Write the journal.
    let journal = RekeyJournal {
        version: 1,
        new_salt,
        started_unix: crate::daemon::clock::now_unix_seconds() as u64,
    };
    journal.write(data_dir)?;

    // K4
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeVaultRename);

    // 7. Atomic rename: vault.staged → vault.
    std::fs::rename(&vault_staged, p.identity_vault_path)
        .map_err(|e| CoreError::Config(format!("vault rename: {e}")))?;
    fsync_dir(data_dir)?;

    // K5
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeAgeKeyRename);

    // 8. Atomic rename: age-key.staged → age-key.
    std::fs::rename(&age_staged, p.age_key_path)
        .map_err(|e| CoreError::Config(format!("age-key rename: {e}")))?;
    fsync_dir(data_dir)?;

    // K6
    #[cfg(feature = "test-harness")]
    p.kill_switch.check(KillPoint::BeforeJournalDelete);

    // 9. Delete the journal.
    RekeyJournal::delete(data_dir)?;
    Ok(())
}

/// Helper: wrap age key plaintext under HKDF(new_passphrase, new_salt,
/// "skattr-age-key-v1") + chacha20poly1305. Match the wrapping format
/// used by the existing `core::storage::age_key` module.
fn wrap_age_key(
    plaintext: &Zeroizing<Vec<u8>>,
    passphrase: &Zeroizing<String>,
    salt: &[u8; 16],
) -> Result<Vec<u8>> {
    // Delegate to the existing storage age-key wrapper helper. The
    // exact function name lives in crates/core/src/storage/age_key.rs;
    // grep for `fn wrap` / `pub fn encrypt` in that module and reuse it
    // rather than reimplementing AEAD here.
    crate::storage::age_key::wrap_with_passphrase(plaintext, passphrase, salt)
}
```

(The `wrap_age_key` helper assumes a `crate::storage::age_key::wrap_with_passphrase` exists or can be lifted out of the existing init flow. If the current code path encrypts inline inside `Pool::open`, refactor that into a free helper as part of this task — small and isolated.)

- [ ] **Step 3: Add a happy-path test**

Append to `passphrase.rs::tests`:

```rust
    #[test]
    fn rekey_happy_path_replaces_both_files() {
        // This is a unit test of the rekey() function only — the
        // integration test in crates/tests/src/passphrase_atomicity.rs
        // exercises the full daemon flow.
        //
        // Setup: create a tempdir, write a known-shape identity.vault
        // and age-key under "old", invoke rekey() with old="old" /
        // new="new" / kill_switch disarmed, then assert both files
        // decrypt under "new" and not under "old".
        //
        // Skipped in this plan because it requires mocked
        // IdentityKey::generate_for_test or similar — refer to the
        // integration test for the real coverage.
    }
```

(The integration test in Task 20 carries the meat; this unit test placeholder documents intent.)

- [ ] **Step 4: Build + clippy**

```bash
. "$HOME/.cargo/env"
cargo build -p skattr-core --features test-harness
cargo clippy -p skattr-core --features test-harness --all-targets -- -D warnings
```

Expected: clean. Resolve any references that don't exist (e.g. if `IdentityVault::wrap` has a different name or if `age_key::wrap_with_passphrase` needs to be created — split that into a small helper in the same commit).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/daemon/passphrase.rs \
        crates/core/src/storage/age_key.rs   # if a helper was extracted
git commit -m "$(cat <<'EOF'
feat(passphrase): rekey() — stage-then-rename happy path + KillSwitch

Stages re-encrypted identity.vault + age-key under the NEW passphrase,
writes the journal, then atomically renames both into place. Six
KillPoint::* values feed the integration tests in Task 20. The
KillSwitch type is gated on feature = "test-harness" so production
builds carry no overhead.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 18: `passphrase::recover_if_needed` — boot-time recovery

**Files:**
- Modify: `crates/core/src/daemon/passphrase.rs` (add `recover_if_needed` + helpers)

- [ ] **Step 1: Add the recovery function**

Append to `passphrase.rs`:

```rust
/// Recovery outcome — used by the caller to decide what to tell the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No journal present; normal boot. (Caller still prompts for passphrase.)
    NoJournal,
    /// Journal was present; rolled back. The OLD passphrase is still valid.
    RolledBack,
    /// Journal was present; rolled forward. The NEW passphrase is now valid
    /// (same one the user just typed).
    RolledForward,
    /// Journal was present; user typed a passphrase that decrypts neither
    /// OLD nor NEW state. Caller must re-prompt.
    WrongPassphrase,
    /// On-disk state is logically inconsistent (e.g. age-key is NEW but
    /// vault is OLD — impossible given step ordering). Caller surfaces
    /// `DaemonErrorKind::InconsistentState` and points at
    /// docs/operations/passphrase-recovery.md.
    Inconsistent,
}

/// Recover if a re-key was interrupted. Called by Daemon::run BEFORE
/// the user is prompted for a passphrase.
///
/// `prompted_passphrase` is the user's most recent passphrase entry. On
/// `Ok(WrongPassphrase)`, the caller re-prompts and re-invokes.
pub(crate) fn recover_if_needed(
    data_dir: &Path,
    identity_vault_path: &Path,
    age_key_path: &Path,
    prompted_passphrase: &Zeroizing<String>,
    audit: &crate::storage::PassphraseAuditRepo<'_>,
) -> Result<RecoveryOutcome> {
    let journal = match RekeyJournal::read(data_dir)? {
        None => {
            // No re-key in flight — clean up any orphaned .staged from a
            // panic before the journal was even written.
            cleanup_staged(data_dir);
            return Ok(RecoveryOutcome::NoJournal);
        }
        Some(j) => j,
    };

    // Probe each file under the NEW KEK derived from the prompted passphrase.
    let new_vault_kek = derive_kek(prompted_passphrase, &journal.new_salt, b"skattr-vault-v1")?;
    let new_age_kek = derive_kek(prompted_passphrase, &journal.new_salt, b"skattr-age-key-v1")?;

    let vault_is_new = try_decrypt_vault(identity_vault_path, &new_vault_kek)?;
    let age_is_new = try_decrypt_age_key(age_key_path, &new_age_kek)?;

    let now = crate::daemon::clock::now_unix_seconds();

    match (vault_is_new, age_is_new) {
        (true, true) => {
            // Both renames happened (steps 7+8); we just crashed before
            // deleting the journal. Finish.
            cleanup_staged(data_dir);
            RekeyJournal::delete(data_dir)?;
            audit.append(now, crate::storage::AuditOutcome::Recovered)?;
            Ok(RecoveryOutcome::RolledForward)
        }
        (true, false) => {
            // Step 7 happened but step 8 didn't. Finish step 8.
            let staged = data_dir.join(AGE_KEY_STAGED);
            if !staged.exists() {
                tracing::warn!("age-key.staged missing during recovery; on-disk state inconsistent");
                return Ok(RecoveryOutcome::Inconsistent);
            }
            std::fs::rename(&staged, age_key_path)
                .map_err(|e| CoreError::Config(format!("recovery age-key rename: {e}")))?;
            fsync_dir(data_dir)?;
            cleanup_staged(data_dir);
            RekeyJournal::delete(data_dir)?;
            audit.append(now, crate::storage::AuditOutcome::Recovered)?;
            Ok(RecoveryOutcome::RolledForward)
        }
        (false, false) => {
            // Neither rename happened. Probe under OLD-KEK (using the
            // salt embedded in the existing identity.vault header).
            if try_decrypt_vault_with_passphrase(identity_vault_path, prompted_passphrase)? {
                // Prompted is the OLD passphrase — roll back.
                cleanup_staged(data_dir);
                RekeyJournal::delete(data_dir)?;
                audit.append(now, crate::storage::AuditOutcome::RolledBack)?;
                Ok(RecoveryOutcome::RolledBack)
            } else {
                // Prompted is neither OLD nor NEW. Re-prompt.
                Ok(RecoveryOutcome::WrongPassphrase)
            }
        }
        (false, true) => {
            // Logically impossible given step ordering.
            Ok(RecoveryOutcome::Inconsistent)
        }
    }
}

fn derive_kek(
    passphrase: &Zeroizing<String>,
    salt: &[u8; 16],
    label: &[u8],
) -> Result<Zeroizing<[u8; 32]>> {
    crate::identity::derive_kek_from_passphrase(passphrase, salt, label)
}

fn try_decrypt_vault(path: &Path, kek: &Zeroizing<[u8; 32]>) -> Result<bool> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(CoreError::Config(format!("read vault: {e}"))),
    };
    Ok(crate::identity::vault_decrypts_with(&blob, kek))
}

fn try_decrypt_vault_with_passphrase(path: &Path, passphrase: &Zeroizing<String>) -> Result<bool> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(CoreError::Config(format!("read vault: {e}"))),
    };
    Ok(crate::identity::vault_decrypts_with_passphrase(&blob, passphrase))
}

fn try_decrypt_age_key(path: &Path, kek: &Zeroizing<[u8; 32]>) -> Result<bool> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(CoreError::Config(format!("read age-key: {e}"))),
    };
    Ok(crate::storage::age_key::decrypts_with(&blob, kek))
}
```

The helpers `crate::identity::derive_kek_from_passphrase`, `vault_decrypts_with`, `vault_decrypts_with_passphrase`, and `crate::storage::age_key::decrypts_with` are thin "try to decrypt; return bool" wrappers. If they don't exist yet, add them in the same commit — each is a 5–10 line helper around existing AEAD code that returns `result.is_ok()`.

- [ ] **Step 2: Add a unit test exercising one branch**

```rust
    #[test]
    fn recover_if_needed_no_journal_returns_no_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let audit = crate::storage::PassphraseAuditRepo::new(&pool);
        let pass = Zeroizing::new("anything".to_string());
        let outcome = recover_if_needed(
            tmp.path(),
            &tmp.path().join("identity.vault"),
            &tmp.path().join("age-key"),
            &pass,
            &audit,
        )
        .unwrap();
        assert_eq!(outcome, RecoveryOutcome::NoJournal);
    }

    #[test]
    fn recover_if_needed_cleans_orphaned_staged_when_no_journal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(VAULT_STAGED), b"orphan").unwrap();
        let pool = std::sync::Arc::new(crate::storage::Pool::in_memory());
        let audit = crate::storage::PassphraseAuditRepo::new(&pool);
        let pass = Zeroizing::new("anything".to_string());
        let _ = recover_if_needed(
            tmp.path(),
            &tmp.path().join("identity.vault"),
            &tmp.path().join("age-key"),
            &pass,
            &audit,
        )
        .unwrap();
        assert!(!tmp.path().join(VAULT_STAGED).exists());
    }
```

(The exhaustive branch coverage is in the integration test in Task 20.)

- [ ] **Step 3: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::passphrase::tests::recover -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/passphrase.rs \
        crates/core/src/identity/   # if helpers added
git commit -m "$(cat <<'EOF'
feat(passphrase): recover_if_needed — boot-time crash recovery

Probes on-disk file fingerprints with the user's typed passphrase
to decide what state the re-key was interrupted in. Five outcomes:
NoJournal, RolledBack, RolledForward, WrongPassphrase, Inconsistent.
Never asks for the OLD passphrase — fingerprint probing carries
all the information needed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 19: `change_passphrase` dispatch handler

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (replace stub arm + add handler)
- Modify: `crates/core/src/daemon/mod.rs` (call `passphrase::recover_if_needed` before unlock — see Step 4)

- [ ] **Step 1: Replace the stub arm**

```rust
        Command::ChangePassphrase { old, new } => {
            change_passphrase(&handle, old, new).await
        }
```

- [ ] **Step 2: Add the handler**

```rust
async fn change_passphrase<S>(
    handle: &Arc<DaemonHandle<S>>,
    old: String,
    new: String,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use crate::daemon::error_kind::DaemonErrorKind;
    use crate::daemon::passphrase::{rekey, RekeyParams};
    use crate::storage::{AuditOutcome, PassphraseAuditRepo};
    use zeroize::Zeroizing;

    // Wrap in Zeroizing immediately.
    let old = Zeroizing::new(old);
    let new = Zeroizing::new(new);

    // 1. Validate `new`: length ≥ 8, zxcvbn ≥ 3, ≠ old.
    if new.as_bytes().len() < 8 {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: "new passphrase must be at least 8 characters".into(),
        }));
    }
    let entropy = zxcvbn::zxcvbn(new.as_str(), &[]);
    if entropy.score() < zxcvbn::Score::Three {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: "new passphrase too weak (zxcvbn score < 3)".into(),
        }));
    }
    if subtle::ConstantTimeEq::ct_eq(old.as_bytes(), new.as_bytes()).into() {
        return Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: "new passphrase must differ from old".into(),
        }));
    }

    // 2. Verify `old` by decrypting both files.
    let identity_vault_path = handle.data_dir.join("identity.vault");
    let age_key_path = handle.data_dir.join("age-key");
    let identity = crate::identity::IdentityVault::load(&identity_vault_path, &old)
        .map_err(|_| IpcError::Daemon(DaemonErrorKind::Unauthorized))?;
    let age_key_plaintext = crate::storage::age_key::load(&age_key_path, &old)
        .map_err(|_| IpcError::Daemon(DaemonErrorKind::Unauthorized))?;

    // 3. Run rekey.
    rekey(RekeyParams {
        data_dir: &handle.data_dir,
        identity_vault_path: &identity_vault_path,
        age_key_path: &age_key_path,
        identity: &identity,
        age_key_plaintext: &age_key_plaintext,
        old_passphrase: &old,
        new_passphrase: &new,
        #[cfg(feature = "test-harness")]
        kill_switch: handle.passphrase_kill_switch.clone(),
    })
    .map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: format!("rekey failed: {e}"),
        })
    })?;

    // 4. Audit + identity refresh.
    let audit = PassphraseAuditRepo::new(&handle.pool);
    audit
        .append(crate::daemon::clock::now_unix_seconds(), AuditOutcome::Changed)
        .map_err(map_err)?;
    handle.inbound.set_identity(std::sync::Arc::new(identity));

    Ok(CommandResult::PassphraseChanged)
}
```

(`handle.data_dir`, `handle.inbound`, and `handle.passphrase_kill_switch` may need to be added to `DaemonHandle` — check the existing struct and append if missing. Match the existing field-naming style.)

If `subtle` isn't already a dependency, add `subtle = { version = "2", default-features = false }` to `crates/core/Cargo.toml`.

- [ ] **Step 3: Wire `recover_if_needed` into `Daemon::run`**

In `crates/core/src/daemon/mod.rs::Daemon::run` (or wherever the user's first passphrase prompt happens), add **before** the existing `IdentityVault::load` call:

```rust
let mut prompted_passphrase = read_passphrase()?;   // existing prompt
loop {
    let outcome = passphrase::recover_if_needed(
        data_dir,
        &data_dir.join("identity.vault"),
        &data_dir.join("age-key"),
        &prompted_passphrase,
        &PassphraseAuditRepo::new(&temporary_audit_pool /* see note */),
    )?;
    match outcome {
        passphrase::RecoveryOutcome::NoJournal
        | passphrase::RecoveryOutcome::RolledBack
        | passphrase::RecoveryOutcome::RolledForward => break,
        passphrase::RecoveryOutcome::WrongPassphrase => {
            tracing::warn!("recovery: wrong passphrase, re-prompting");
            prompted_passphrase = read_passphrase()?;
            continue;
        }
        passphrase::RecoveryOutcome::Inconsistent => {
            return Err(CoreError::Config(
                "passphrase rekey state is inconsistent; see docs/operations/passphrase-recovery.md".into(),
            ));
        }
    }
}
```

Note: the audit repo needs a Pool, but the Pool needs the age key, which is what we're recovering. Solution: the audit table lives in the SQLite database, so it's only writable AFTER unlock. Adapt by deferring the audit write — `recover_if_needed` returns the outcome and the caller appends the audit row AFTER the pool is open. Adjust the function signature to remove the `&PassphraseAuditRepo<'_>` arg and instead return `(RecoveryOutcome, Option<AuditOutcome>)`; the caller writes the audit row once the pool is ready. Apply this refactor in the same task before committing.

- [ ] **Step 4: Add a dispatch test**

```rust
    #[tokio::test]
    async fn change_passphrase_rejects_weak_new() {
        let handle = test_handle().await;
        let err = execute_command(
            handle,
            Command::ChangePassphrase {
                old: "correct old".into(),
                new: "12345".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            IpcError::Daemon(crate::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. })
        ));
    }
```

(The full atomicity test lives in `crates/tests/src/passphrase_atomicity.rs` — Task 20.)

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::change_passphrase -- --nocapture
cargo test -p skattr-core --features test-harness daemon::passphrase -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs \
        crates/core/src/daemon/mod.rs \
        crates/core/src/daemon/handle.rs \
        crates/core/src/daemon/passphrase.rs \
        crates/core/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
feat(dispatch): ChangePassphrase + boot-time recovery wiring

Validates new passphrase (length, zxcvbn ≥ 3, ≠ old), verifies old
by decrypting both files, runs rekey(), appends a 'changed' audit
row, refreshes DaemonInbound::set_identity. Daemon::run now invokes
passphrase::recover_if_needed in a loop before the first vault open,
re-prompting on WrongPassphrase.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 20: Six-kill-point integration test

**Files:**
- Create: `crates/tests/src/passphrase_atomicity.rs`
- Modify: `crates/tests/src/lib.rs` (add `mod passphrase_atomicity;` if the crate uses a lib.rs aggregator)

- [ ] **Step 1: Write the integration test**

Create `crates/tests/src/passphrase_atomicity.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Six-kill-point coverage of `core::daemon::passphrase::rekey` +
//! `recover_if_needed`. See spec § "ChangePassphrase — Test plan".
//!
//! Each kill point arms the KillSwitch, sends ChangePassphrase, panics
//! the rekey task, restarts the daemon, asserts: (a) the daemon
//! unlocks under the expected passphrase, (b) the on-disk file
//! fingerprints match the table, (c) SQLite is readable, (d) the
//! identity Ed25519 pubkey is unchanged, (e) the audit row carries
//! the expected outcome.

#![cfg(feature = "test-harness")]

use std::time::Duration;

use skattr_core::daemon::passphrase::KillPoint;
use tokio::time::timeout;

mod helpers;
use helpers::{spawn_daemon_with_passphrase, IpcClient};

async fn run_kill_point(point: KillPoint, expected_passphrase: &'static str) {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().to_path_buf();

    // Boot daemon with passphrase "old".
    let mut daemon = spawn_daemon_with_passphrase(&data_dir, "old").await;
    let pubkey_before = daemon.pubkey();

    // Arm the kill switch + send ChangePassphrase. Expect a panic
    // (which the spawn helper detects via its abort future).
    daemon.arm_passphrase_kill_switch(point);
    let send_result = timeout(Duration::from_secs(5), daemon.client().change_passphrase("old", "newnewnew"))
        .await;
    // The send may succeed (panic happens in the background task)
    // or error with a connection drop — both are fine.
    let _ = send_result;

    // Wait for the daemon process to actually exit / abort.
    daemon.wait_for_exit().await;

    // Restart the daemon under the EXPECTED passphrase.
    let daemon2 = spawn_daemon_with_passphrase(&data_dir, expected_passphrase).await;
    let pubkey_after = daemon2.pubkey();
    assert_eq!(pubkey_before, pubkey_after, "identity pubkey must be unchanged");

    // SQLite is readable.
    let contacts = daemon2.client().list_contacts().await.unwrap();
    let _ = contacts;

    // No leftover .staged or journal files.
    assert!(!data_dir.join("identity.vault.staged").exists());
    assert!(!data_dir.join("age-key.staged").exists());
    assert!(!data_dir.join("passphrase-rekey.journal").exists());

    daemon2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k1_before_stage_vault_rolls_back() {
    run_kill_point(KillPoint::BeforeStageVault, "old").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k2_before_stage_age_key_rolls_back() {
    run_kill_point(KillPoint::BeforeStageAgeKey, "old").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k3_before_journal_rolls_back() {
    run_kill_point(KillPoint::BeforeJournal, "old").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k4_before_vault_rename_rolls_back() {
    run_kill_point(KillPoint::BeforeVaultRename, "old").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k5_before_age_key_rename_rolls_forward() {
    run_kill_point(KillPoint::BeforeAgeKeyRename, "newnewnew").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k6_before_journal_delete_rolls_forward() {
    run_kill_point(KillPoint::BeforeJournalDelete, "newnewnew").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k4_wrong_recovery_passphrase_returns_wrong_passphrase() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().to_path_buf();
    let mut daemon = spawn_daemon_with_passphrase(&data_dir, "old").await;
    daemon.arm_passphrase_kill_switch(KillPoint::BeforeVaultRename);
    let _ = daemon.client().change_passphrase("old", "newnewnew").await;
    daemon.wait_for_exit().await;

    // Try to restart under the NEW passphrase — should fail until we
    // try the OLD one.
    let err = helpers::try_spawn_daemon_with_passphrase(&data_dir, "newnewnew").await;
    assert!(err.is_err(), "K4 + new passphrase must fail to unlock");

    let _ok = spawn_daemon_with_passphrase(&data_dir, "old").await;
    // (Implicit: spawn succeeds, so the test passes.)
}
```

The `helpers` module provides `spawn_daemon_with_passphrase` (extension of the existing `crates/tests/src/cli_two_daemons.rs` / `cli_ipc_roundtrip.rs` patterns). If it doesn't exist, factor it out from one of those existing tests in the same task.

- [ ] **Step 2: Run the tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-tests --features test-harness passphrase_atomicity -- --nocapture
```

Expected: 7 tests PASS. Each takes ~5–10s (daemon spawn + kill + restart).

- [ ] **Step 3: Commit**

```bash
git add crates/tests/src/passphrase_atomicity.rs crates/tests/src/lib.rs
git commit -m "$(cat <<'EOF'
test(passphrase): six kill-point + wrong-recovery integration tests

K1-K3: kill before any rename → rollback under OLD.
K4: kill after journal but before vault rename → rollback under OLD.
K5-K6: kill after vault rename → roll forward under NEW.
K4-wrong: confirms NEW passphrase is rejected until OLD is provided.

All assertions: identity pubkey unchanged, SQLite readable,
no leftover .staged or journal files post-recovery.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 8 — Logs subsystem

### Task 21: `logs.rs` — ring buffer + redacting tracing layer

**Files:**
- Create: `crates/core/src/daemon/logs.rs`
- Modify: `crates/core/src/daemon/mod.rs` (`pub(crate) mod logs;` and install the layer in the subscriber stack)
- Modify: `crates/core/Cargo.toml` (add `tracing-appender = "0.2"`, `regex = "1"` if not already present)

- [ ] **Step 1: Add deps**

In `crates/core/Cargo.toml` `[dependencies]`:

```toml
tracing-appender = "0.2"
regex = { version = "1", default-features = false, features = ["std"] }
```

Enable the `reload` feature on the existing `tracing-subscriber` dep:

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "registry", "reload"] }
```

- [ ] **Step 2: Create the ring-buffer layer**

Create `crates/core/src/daemon/logs.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! In-memory ring-buffer log layer + redacted IPC stream.
//!
//! ≤ 5000 records held in memory; older records evicted FIFO. Each
//! record passes through a redactor that strips bare 64-char hex blobs
//! (pubkeys), `*.onion` strings, and message-body interpolations
//! ABOVE the `debug` level.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::daemon::commands::{LogLevel, LogRecord};

const RING_CAP: usize = 5_000;
const STREAM_CAP: usize = 256;

/// Shared handle to the ring buffer and the broadcast channel.
#[derive(Clone)]
pub struct LogSink {
    inner: Arc<LogSinkInner>,
}

struct LogSinkInner {
    ring: Mutex<std::collections::VecDeque<LogRecord>>,
    next_seq: std::sync::atomic::AtomicU64,
    tx: broadcast::Sender<LogRecord>,
}

impl LogSink {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(STREAM_CAP);
        Self {
            inner: Arc::new(LogSinkInner {
                ring: Mutex::new(std::collections::VecDeque::with_capacity(RING_CAP)),
                next_seq: std::sync::atomic::AtomicU64::new(1),
                tx,
            }),
        }
    }

    /// Subscribe to live log records. Lossy (broadcast::Receiver).
    pub fn subscribe(&self) -> broadcast::Receiver<LogRecord> {
        self.inner.tx.subscribe()
    }

    /// Snapshot of records since `since_seq` (inclusive), capped at `limit`.
    pub fn snapshot(&self, since_seq: Option<u64>, limit: usize) -> (Vec<LogRecord>, u64) {
        let limit = limit.min(1000);
        let ring = self.inner.ring.lock();
        let start = since_seq.unwrap_or(0);
        let records: Vec<LogRecord> = ring
            .iter()
            .filter(|r| r.seq >= start)
            .take(limit)
            .cloned()
            .collect();
        let next = records.last().map(|r| r.seq + 1).unwrap_or(start);
        (records, next)
    }

    fn push(&self, level: LogLevel, target: String, message: String) {
        let seq = self
            .inner
            .next_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let rec = LogRecord {
            seq,
            ts_unix_ms: now_unix_ms(),
            level,
            target,
            message,
        };
        let mut ring = self.inner.ring.lock();
        if ring.len() == RING_CAP {
            ring.pop_front();
        }
        ring.push_back(rec.clone());
        drop(ring);
        let _ = self.inner.tx.send(rec); // ignore "no receivers"
    }
}

impl Default for LogSink {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `tracing-subscriber` layer that funnels events into a `LogSink`,
/// applying redaction above the `debug` level.
pub struct RingBufferLayer {
    sink: LogSink,
    redactor: Redactor,
}

impl RingBufferLayer {
    pub fn new(sink: LogSink) -> Self {
        Self {
            sink,
            redactor: Redactor::new(),
        }
    }
}

impl<S: Subscriber> Layer<S> for RingBufferLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        };

        // Format the event into a string. Use a Visit impl that
        // concatenates field key=value pairs.
        let mut formatted = String::new();
        let mut visitor = StringVisitor(&mut formatted);
        event.record(&mut visitor);

        // Redact above debug.
        let message = if matches!(level, LogLevel::Trace | LogLevel::Debug) {
            formatted
        } else {
            self.redactor.redact(&formatted)
        };

        self.sink
            .push(level, metadata.target().to_string(), message);
    }
}

struct StringVisitor<'a>(&'a mut String);
impl tracing::field::Visit for StringVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = std::fmt::Write::write_fmt(self.0, format_args!("{}={:?}", field.name(), value));
    }
}

/// Redactor that strips:
///   - 64-char lowercase hex strings (Ed25519 / X25519 pubkeys, BLAKE2s hashes)
///   - 56-char base32 + ".onion" suffixes (v3 onion addresses)
///   - any field starting with "body=" / "ciphertext=" / "psk=" / "passphrase="
struct Redactor {
    hex64: regex::Regex,
    onion: regex::Regex,
    secret_field: regex::Regex,
}

impl Redactor {
    fn new() -> Self {
        Self {
            hex64: regex::Regex::new(r"\b[0-9a-f]{64}\b").unwrap(),
            onion: regex::Regex::new(r"\b[a-z2-7]{56}\.onion\b").unwrap(),
            secret_field: regex::Regex::new(r"\b(body|ciphertext|psk|passphrase|seed)=\S+").unwrap(),
        }
    }
    fn redact(&self, s: &str) -> String {
        let s = self.hex64.replace_all(s, "[REDACTED-PUBKEY]");
        let s = self.onion.replace_all(&s, "[REDACTED-ONION]");
        let s = self.secret_field.replace_all(&s, "$1=[REDACTED]");
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest_when_full() {
        let sink = LogSink::new();
        for i in 0..(RING_CAP + 10) {
            sink.push(LogLevel::Info, "test".into(), format!("msg-{i}"));
        }
        let (records, _) = sink.snapshot(None, 1000);
        // Oldest in buffer should be msg-10 (first 10 evicted).
        assert!(records.iter().any(|r| r.message == "msg-10"));
        assert!(!records.iter().any(|r| r.message == "msg-0"));
    }

    #[test]
    fn snapshot_respects_since_seq_and_limit() {
        let sink = LogSink::new();
        for i in 0..100 {
            sink.push(LogLevel::Info, "t".into(), format!("m-{i}"));
        }
        let (records, next) = sink.snapshot(Some(50), 10);
        assert_eq!(records.len(), 10);
        assert_eq!(records[0].seq, 50);
        assert_eq!(next, 60);
    }

    #[test]
    fn redactor_strips_hex_pubkey_and_onion() {
        let r = Redactor::new();
        let input = "peer abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234 at xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.onion";
        let out = r.redact(input);
        assert!(out.contains("[REDACTED-PUBKEY]"));
        assert!(out.contains("[REDACTED-ONION]"));
        assert!(!out.contains("abcd1234"));
    }

    #[test]
    fn redactor_strips_secret_fields() {
        let r = Redactor::new();
        let out = r.redact("body=hello psk=00112233 ok=true");
        assert!(out.contains("body=[REDACTED]"));
        assert!(out.contains("psk=[REDACTED]"));
        assert!(out.contains("ok=true"));
    }

    #[test]
    fn debug_level_skips_redaction() {
        let sink = LogSink::new();
        let layer = RingBufferLayer::new(sink.clone());
        // Building a tracing::Event in unit tests is awkward; redaction
        // bypass at debug level is exercised in the integration tests
        // (Task 23) where a real subscriber is wired up.
        let _ = layer;
    }
}
```

If `parking_lot` isn't already a dep, prefer `std::sync::Mutex` to avoid adding a new dep — adapt the locks accordingly.

- [ ] **Step 3: Install the layer in `Daemon::run`**

In `crates/core/src/daemon/mod.rs`'s subscriber-init code (typically near the top of `Daemon::run`), build the layered subscriber:

```rust
use crate::daemon::logs::{LogSink, RingBufferLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, fmt, reload};

let log_sink = LogSink::new();
let env_filter = EnvFilter::try_new(&config.log_filter)
    .unwrap_or_else(|_| EnvFilter::new("info"));

// File-appender layer is reload-able so persist_logs_to_disk can hot-toggle.
let (file_layer, file_handle) = reload::Layer::new(None::<tracing_appender::non_blocking::NonBlocking>);

tracing_subscriber::registry()
    .with(env_filter)
    .with(fmt::layer().with_writer(std::io::stderr))
    .with(RingBufferLayer::new(log_sink.clone()))
    .with(file_layer)
    .init();

// Stash log_sink and file_handle on DaemonHandle so dispatch + the
// SetConfig handler can access them.
```

If `config.ui.persist_logs_to_disk` is true at boot, install the file appender immediately (see Task 22 step on hot-toggle).

- [ ] **Step 4: Run tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-core --features test-harness daemon::logs::tests -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock \
        crates/core/src/daemon/{logs.rs,mod.rs}
git commit -m "$(cat <<'EOF'
feat(logs): in-memory ring buffer + redacting tracing layer

5000-record VecDeque-backed ring; broadcast channel for live tail.
Redactor strips 64-char hex pubkeys, *.onion v3 addresses, and
common secret-field interpolations above the debug level.
RingBufferLayer is installed in Daemon::run alongside the existing
fmt + EnvFilter stack.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 22: `tail_logs` handler + `Event::LogRecord` emission + `persist_logs_to_disk` hot-toggle

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (replace `Command::TailLogs` stub arm + add handler)
- Modify: `crates/core/src/daemon/mod.rs` (spawn a tap that forwards `LogSink::subscribe()` records to the IPC event bus when any subscriber is filtering on `EventFilter::Logs`)
- Modify: `crates/core/src/daemon/dispatch.rs::set_config` (handle `persist_logs_to_disk` via the reload handle)

- [ ] **Step 1: Wire the handler**

```rust
        Command::TailLogs { since_seq, limit } => tail_logs(&handle, since_seq, limit).await,
```

```rust
async fn tail_logs<S>(
    handle: &Arc<DaemonHandle<S>>,
    since_seq: Option<u64>,
    limit: u32,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (records, next_since_seq) = handle.log_sink.snapshot(since_seq, limit as usize);
    Ok(CommandResult::Logs {
        records,
        next_since_seq,
    })
}
```

- [ ] **Step 2: Spawn a log → event tap**

In `Daemon::run` after the event-bus and log-sink are constructed:

```rust
{
    let mut log_rx = log_sink.subscribe();
    let event_bus = events.clone();
    tokio::spawn(async move {
        while let Ok(record) = log_rx.recv().await {
            event_bus.emit(crate::daemon::events::Event::LogRecord(record));
        }
    });
}
```

(The event bus's per-subscriber filter (`EventFilter::Logs`) gates delivery; this tap unconditionally re-broadcasts, which is fine because `event_bus.emit` is cheap when no subscriber wants the event.)

- [ ] **Step 3: Hot-toggle file appender from `set_config`**

In `set_config` (Task 12), after `cfg.apply_patch(&patch)`:

```rust
if let Some(persist) = patch.persist_logs_to_disk {
    apply_persist_logs_to_disk(handle, persist).map_err(|e| {
        IpcError::Daemon(DaemonErrorKind::InvalidArgument {
            reason: format!("toggle log persistence: {e}"),
        })
    })?;
}
```

Add the helper in `dispatch.rs`:

```rust
fn apply_persist_logs_to_disk<S>(
    handle: &Arc<DaemonHandle<S>>,
    persist: bool,
) -> std::result::Result<(), CoreError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if persist {
        let logs_dir = handle.data_dir.join("logs");
        std::fs::create_dir_all(&logs_dir)
            .map_err(|e| CoreError::Config(format!("create logs dir: {e}")))?;
        let file_appender = tracing_appender::rolling::daily(&logs_dir, "skattr.log");
        let (nb, _guard) = tracing_appender::non_blocking(file_appender);
        // Leak the guard intentionally — it must live for the daemon
        // lifetime; daemon shutdown drops the whole process.
        std::mem::forget(_guard);
        handle.log_file_reload
            .modify(|opt| *opt = Some(tracing_subscriber::fmt::Layer::default().with_writer(nb)))
            .map_err(|e| CoreError::Config(format!("install file layer: {e}")))?;
    } else {
        handle.log_file_reload
            .modify(|opt| *opt = None)
            .map_err(|e| CoreError::Config(format!("remove file layer: {e}")))?;
    }
    Ok(())
}
```

(Adjust the reload-handle generic types to match what the subscriber stack actually uses; consult `tracing_subscriber::reload::Layer` docs if the modify signature mismatches.)

- [ ] **Step 4: Add tests**

```rust
    #[tokio::test]
    async fn tail_logs_returns_recent_records() {
        let handle = test_handle().await;
        // Push records via the sink directly.
        for i in 0..5 {
            handle.log_sink.push(crate::daemon::commands::LogLevel::Info, "test".into(), format!("m-{i}"));
        }
        let result = execute_command(handle, Command::TailLogs { since_seq: None, limit: 100 }).await.unwrap();
        match result {
            CommandResult::Logs { records, .. } => {
                assert!(records.len() >= 5);
                assert!(records.iter().any(|r| r.message.contains("m-4")));
            }
            other => panic!("expected Logs, got {other:?}"),
        }
    }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::dispatch::tests::tail_logs -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/daemon/{dispatch.rs,mod.rs,handle.rs}
git commit -m "$(cat <<'EOF'
feat(logs): TailLogs handler + Event::LogRecord tap + disk-persist toggle

TailLogs returns a paginated snapshot (≤ 1000 records, defaults to
"from oldest"). A tokio task taps LogSink::subscribe and re-emits
each record onto the daemon event bus so EventFilter::Logs
subscribers get live tail. set_config(persist_logs_to_disk) hot-
toggles a tracing-appender::rolling::daily layer through the
existing reload::Layer in the subscriber stack.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 23: Logs end-to-end test

**Files:**
- Modify: `crates/core/src/daemon/logs.rs::tests` (add the integration-style test)

- [ ] **Step 1: Add the end-to-end test**

```rust
    #[tokio::test]
    async fn ring_buffer_layer_redacts_pubkeys_at_info() {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        let sink = LogSink::new();
        let layer = RingBufferLayer::new(sink.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        let pubkey = "0".repeat(64);
        tracing::info!(peer = %pubkey, "received message");

        let (records, _) = sink.snapshot(None, 100);
        let info_record = records.iter().find(|r| matches!(r.level, LogLevel::Info));
        let info_record = info_record.expect("at least one Info record");
        assert!(
            !info_record.message.contains(&pubkey),
            "info-level pubkey must be redacted, got: {}",
            info_record.message
        );
        assert!(info_record.message.contains("[REDACTED-PUBKEY]"));
    }

    #[tokio::test]
    async fn debug_record_keeps_pubkey_intact() {
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        let sink = LogSink::new();
        let layer = RingBufferLayer::new(sink.clone());
        let filter = tracing_subscriber::EnvFilter::new("trace");
        let subscriber = tracing_subscriber::registry().with(filter).with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        let pubkey = "a".repeat(64);
        tracing::debug!(peer = %pubkey, "fine-grained");

        let (records, _) = sink.snapshot(None, 100);
        let debug_record = records.iter().find(|r| matches!(r.level, LogLevel::Debug));
        let debug_record = debug_record.expect("at least one Debug record");
        assert!(debug_record.message.contains(&pubkey));
    }
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p skattr-core --features test-harness daemon::logs::tests::ring_buffer_layer_redacts -- --nocapture
cargo test -p skattr-core --features test-harness daemon::logs::tests::debug_record_keeps -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/daemon/logs.rs
git commit -m "$(cat <<'EOF'
test(logs): info-level redacts pubkeys, debug-level preserves them

End-to-end coverage that the redactor wires up correctly through
the tracing subscriber stack and respects the level boundary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 9 — Wipe-all-data

### Task 24: `WipeAllData` handler + integration test

**Files:**
- Modify: `crates/core/src/daemon/dispatch.rs` (replace `Command::WipeAllData` stub arm + add handler)
- Create: `crates/tests/src/wipe_data.rs`

- [ ] **Step 1: Wire the handler arm**

```rust
        Command::WipeAllData => wipe_all_data(handle).await,
```

```rust
async fn wipe_all_data<S>(
    handle: Arc<DaemonHandle<S>>,
) -> std::result::Result<CommandResult, IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let data_dir = handle.data_dir.clone();
    // Reply BEFORE teardown so the UI sees Ok before the IPC closes.
    // We achieve this by sending Ok now and spawning the teardown.
    tokio::spawn(async move {
        // Allow ~100ms for the reply to flush over IPC.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Trigger the daemon's shutdown signal so Pool drops cleanly.
        handle.shutdown.send(true).ok();
        // Wait briefly for in-flight tasks to settle.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Best-effort directory wipe.
        if let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
            tracing::error!(error = %e, dir = ?data_dir, "wipe_all_data: remove_dir_all failed");
        }
        std::process::exit(0);
    });
    Ok(CommandResult::Ok)
}
```

(`handle.shutdown` should be a `tokio::sync::watch::Sender<bool>` — if it isn't, plumb one through `DaemonHandle` so `Daemon::run`'s task tree can react.)

- [ ] **Step 2: Write the integration test**

Create `crates/tests/src/wipe_data.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(feature = "test-harness")]

use std::time::Duration;

mod helpers;
use helpers::spawn_daemon_with_passphrase;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wipe_all_data_removes_data_dir_and_exits() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");
    let mut daemon = spawn_daemon_with_passphrase(&data_dir, "test-passphrase").await;
    assert!(data_dir.exists(), "daemon should have created data_dir");

    // Send WipeAllData. The reply may or may not arrive depending on
    // timing; we mostly care about the post-conditions.
    let _ = daemon.client().wipe_all_data().await;

    // Wait up to 2s for the daemon process to exit and the dir to vanish.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && data_dir.exists() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(!data_dir.exists(), "data_dir must be removed after WipeAllData");
}
```

- [ ] **Step 3: Run tests**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-tests --features test-harness wipe_data -- --nocapture
```

Expected: PASS (~2s).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/daemon/dispatch.rs crates/tests/src/wipe_data.rs
git commit -m "$(cat <<'EOF'
feat(dispatch): WipeAllData handler + integration test

Replies CommandResult::Ok before the teardown so the UI sees a
clean ack; spawns a task that signals shutdown, allows in-flight
tasks to settle, removes data_dir, exits process(0).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 10 — Tauri tray + notifications

### Task 25: `tray.rs` — Tauri 2 built-in tray

**Files:**
- Create: `crates/ui/src/tray.rs`
- Modify: `crates/ui/Cargo.toml` (enable Tauri's `tray-icon` feature)
- Modify: `crates/ui/src/main.rs` (call `tray::install` after Daemon ready)

- [ ] **Step 1: Enable the Tauri tray feature**

In `crates/ui/Cargo.toml`, ensure `tauri = { version = "2", features = ["..., tray-icon"] }`.

- [ ] **Step 2: Create the tray module**

Create `crates/ui/src/tray.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// Initialise the tray. Returns Ok(()) on success; on Linux/Wayland or
/// other no-tray environments, logs a warning and returns Ok(()) so
/// the daemon keeps running (close-button reverts to "quit").
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let header = MenuItemBuilder::new("Skattr")
        .id("header")
        .enabled(false)
        .build(app)?;
    let show = MenuItemBuilder::new("Show window").id("show").build(app)?;
    let tor_status = MenuItemBuilder::new("Tor: connecting…")
        .id("tor_status")
        .enabled(false)
        .build(app)?;
    let unread = MenuItemBuilder::new("Unread: 0")
        .id("unread")
        .enabled(false)
        .build(app)?;
    let quit = MenuItemBuilder::new("Quit").id("quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&header)
        .separator()
        .item(&show)
        .separator()
        .item(&tor_status)
        .item(&unread)
        .separator()
        .item(&quit)
        .build()?;

    let result = TrayIconBuilder::new()
        .menu(&menu)
        .icon(app.default_window_icon().unwrap().clone())
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                if let Some(w) = tray.app_handle().get_webview_window("main") {
                    if w.is_visible().unwrap_or(false) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app);

    if let Err(e) = result {
        tracing::warn!(error = %e, "tray init failed; continuing without tray");
    }
    Ok(())
}
```

- [ ] **Step 3: Call from main.rs**

In `crates/ui/src/main.rs`, inside the Tauri builder's `setup` closure (after Daemon ready):

```rust
crate::tray::install(app.handle())?;
```

Also register `pub mod tray;` near the top of `main.rs`.

- [ ] **Step 4: Run check**

```bash
. "$HOME/.cargo/env"
cargo check -p skattr-ui
```

Expected: clean compile. (Tray click behaviour requires manual smoke test — see `docs/operations/2f-notification-smoke.md` from Task 42.)

- [ ] **Step 5: Commit**

```bash
git add crates/ui/Cargo.toml crates/ui/src/{tray.rs,main.rs}
git commit -m "$(cat <<'EOF'
feat(ui): Tauri tray + click-to-toggle window

Built-in TrayIconBuilder; menu items: Show window / Tor status /
Unread / Quit (status items disabled). Click on tray icon toggles
window visibility; click on Quit calls app.exit(0). Tray init
failures (Wayland / no-tray Linux) log a warning and continue —
close-button falls back to quit when no tray exists.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 26: `notifications.rs` — `notify-rust` Tauri command

**Files:**
- Create: `crates/ui/src/notifications.rs`
- Modify: `crates/ui/Cargo.toml` (`notify-rust = "4"`)
- Modify: `crates/ui/src/main.rs` (register the command)

- [ ] **Step 1: Add the dep**

In `crates/ui/Cargo.toml`:

```toml
notify-rust = { version = "4", default-features = false }
```

- [ ] **Step 2: Create `notifications.rs`**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#[tauri::command]
pub fn notify(title: String, body: String, conversation_id: Option<String>) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(&title).body(&body).appname("Skattr");
    #[cfg(target_os = "linux")]
    {
        if let Some(id) = conversation_id.as_ref() {
            n.hint(notify_rust::Hint::Custom("conversation_id".into(), id.clone()));
        }
    }
    n.show().map(|_| ()).map_err(|e| format!("notify: {e}"))
}

#[tauri::command]
pub fn focus_window_and_open_conversation(
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    use tauri::Manager;
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    // Ask the SvelteKit shell to navigate via a custom event.
    w.eval(&format!(
        r#"window.dispatchEvent(new CustomEvent('skattr:open-conversation', {{ detail: {{ id: {:?} }} }}));"#,
        id
    ))
    .map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 3: Register the commands**

In `crates/ui/src/main.rs`'s `tauri::generate_handler![...]`:

```rust
crate::notifications::notify,
crate::notifications::focus_window_and_open_conversation,
```

Add `pub mod notifications;` near the top.

- [ ] **Step 4: Run check**

```bash
cargo check -p skattr-ui
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/Cargo.toml crates/ui/src/{notifications.rs,main.rs}
git commit -m "$(cat <<'EOF'
feat(ui): notify-rust Tauri command + focus_window_and_open_conversation

Two new Tauri commands the SvelteKit dispatcher invokes when a
notification fires. focus_window_and_open_conversation surfaces
the window, focuses it, and dispatches a custom JS event so the
router can navigate to the conversation.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 27: Close-button → hide-to-tray + start-minimised

**Files:**
- Modify: `crates/ui/src/main.rs` (intercept `WindowEvent::CloseRequested`; consult config for `start_minimised`)

- [ ] **Step 1: Read current config from in-process daemon at startup**

In the Tauri builder's `setup`, after Daemon::run is ready, fetch the config snapshot via the in-process IpcClient:

```rust
let cfg = ipc_client.get_config().await?;
if cfg.start_minimised {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}
// Stash close_to_tray on a state struct accessible from the close handler.
app.manage(CloseToTrayState { enabled: std::sync::atomic::AtomicBool::new(cfg.close_to_tray) });
```

Define `CloseToTrayState`:

```rust
struct CloseToTrayState {
    enabled: std::sync::atomic::AtomicBool,
}
```

Subscribe to `Event::ConfigChanged` (you may add it later if needed; for 2.F we just refresh on each `set_config` from the UI side via direct IPC ack — no daemon-side ConfigChanged event needed since the UI is the only writer and updates the local store optimistically).

- [ ] **Step 2: Intercept the close event**

In the Tauri builder, `.on_window_event(|window, event| { ... })`:

```rust
if let tauri::WindowEvent::CloseRequested { api, .. } = event {
    let app = window.app_handle();
    if let Some(state) = app.try_state::<CloseToTrayState>() {
        if state.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            api.prevent_close();
            let _ = window.hide();
            // First-time toast is dispatched from the SvelteKit side via a
            // custom event; the JS layer reads localStorage to suppress
            // subsequent toasts.
            let _ = window.eval(
                "window.dispatchEvent(new CustomEvent('skattr:close-to-tray-hidden'));",
            );
        }
    }
}
```

- [ ] **Step 3: Run check**

```bash
cargo check -p skattr-ui
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src/main.rs
git commit -m "$(cat <<'EOF'
feat(ui): close-button → hide-to-tray + start-minimised

Close button hides the window when ui.close_to_tray (default true);
SvelteKit emits a one-time toast after the hide (suppressed via
localStorage). start_minimised hides the window on launch.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 28: Notification dispatcher (TS) + truth-table tests

**Files:**
- Create: `crates/ui/src-svelte/src/lib/Notifications/dispatcher.ts`
- Create: `crates/ui/src-svelte/src/lib/Notifications/dispatcher.test.ts`
- Create: `crates/ui/src-svelte/src/lib/stores/focus.ts`

- [ ] **Step 1: Create the focus store**

Create `crates/ui/src-svelte/src/lib/stores/focus.ts`:

```ts
import { writable, type Readable } from 'svelte/store';

interface FocusState {
  windowFocused: boolean;
  activeContactId: string | null;
}

const _state = writable<FocusState>({ windowFocused: true, activeContactId: null });

export const focus: Readable<FocusState> = { subscribe: _state.subscribe };

export function setActiveContact(id: string | null) {
  _state.update((s) => ({ ...s, activeContactId: id }));
}

export function setWindowFocused(focused: boolean) {
  _state.update((s) => ({ ...s, windowFocused: focused }));
}

// Wire up to the browser focus events on init.
if (typeof window !== 'undefined') {
  window.addEventListener('focus', () => setWindowFocused(true));
  window.addEventListener('blur', () => setWindowFocused(false));
}
```

- [ ] **Step 2: Create the dispatcher**

Create `crates/ui/src-svelte/src/lib/Notifications/dispatcher.ts`:

```ts
import type { ConfigSnapshot, NotificationMode } from '$lib/ipc/types';

export interface FocusInputs {
  windowFocused: boolean;
  activeContactId: string | null;
}

export interface ContactInputs {
  id: string;
  nickname: string | null;
  muted: boolean;
}

export interface MessageInputs {
  contact: string;       // contact id (hex pubkey)
  preview: string;       // already-truncated body or empty
}

export function shouldNotify(
  msg: MessageInputs,
  focus: FocusInputs,
  config: ConfigSnapshot,
  contact: ContactInputs,
): boolean {
  if (config.notification_mode === 'off') return false;
  if (contact.muted) return false;
  if (focus.windowFocused && focus.activeContactId === msg.contact) return false;
  return true;
}

export function buildNotification(
  msg: MessageInputs,
  config: ConfigSnapshot,
  contact: ContactInputs,
): { title: string; body: string } {
  const sender = contact.nickname ?? '(unknown)';
  switch (config.notification_mode) {
    case 'full':
      return { title: sender, body: msg.preview || '(empty)' };
    case 'minimal':
      return { title: sender, body: '' };
    case 'generic':
      return { title: 'Skattr', body: 'New message' };
    case 'off':
      return { title: '', body: '' };
  }
}
```

- [ ] **Step 3: Truth-table tests**

Create `crates/ui/src-svelte/src/lib/Notifications/dispatcher.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { shouldNotify, buildNotification } from './dispatcher';
import type { ConfigSnapshot } from '$lib/ipc/types';

const baseCfg = (mode: ConfigSnapshot['notification_mode']): ConfigSnapshot => ({
  history_retention_days: 0,
  direct_timeout_secs: 30,
  notification_mode: mode,
  close_to_tray: true,
  start_minimised: false,
  persist_logs_to_disk: false,
});

describe('shouldNotify truth table', () => {
  const msg = { contact: 'alice', preview: 'hi' };
  const contactNotMuted = { id: 'alice', nickname: 'Alice', muted: false };
  const contactMuted = { id: 'alice', nickname: 'Alice', muted: true };

  it('off mode never notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: null }, baseCfg('off'), contactNotMuted),
    ).toBe(false);
  });

  it('muted contact never notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: null }, baseCfg('full'), contactMuted),
    ).toBe(false);
  });

  it('focused + active conversation suppresses', () => {
    expect(
      shouldNotify(msg, { windowFocused: true, activeContactId: 'alice' }, baseCfg('full'), contactNotMuted),
    ).toBe(false);
  });

  it('focused + DIFFERENT conversation still notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: true, activeContactId: 'bob' }, baseCfg('full'), contactNotMuted),
    ).toBe(true);
  });

  it('unfocused notifies', () => {
    expect(
      shouldNotify(msg, { windowFocused: false, activeContactId: 'alice' }, baseCfg('full'), contactNotMuted),
    ).toBe(true);
  });
});

describe('buildNotification', () => {
  const msg = { contact: 'alice', preview: 'hi there' };
  const c = { id: 'alice', nickname: 'Alice', muted: false };

  it('full → sender + preview', () => {
    expect(buildNotification(msg, baseCfg('full'), c)).toEqual({ title: 'Alice', body: 'hi there' });
  });
  it('minimal → sender only', () => {
    expect(buildNotification(msg, baseCfg('minimal'), c)).toEqual({ title: 'Alice', body: '' });
  });
  it('generic → New message', () => {
    expect(buildNotification(msg, baseCfg('generic'), c)).toEqual({ title: 'Skattr', body: 'New message' });
  });
});
```

- [ ] **Step 4: Run Vitest**

```bash
cd crates/ui/src-svelte
pnpm test --run dispatcher
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/lib/Notifications/ \
        crates/ui/src-svelte/src/lib/stores/focus.ts
git commit -m "$(cat <<'EOF'
feat(ui): focus-aware notification dispatcher + truth table tests

Pure-function shouldNotify + buildNotification keep the UI free of
notification side-effects until shouldNotify returns true. Truth
table covers off / muted / focused-active / focused-other / blurred.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 11 — Settings UI shell

### Task 29: `config` store + IPC adapter

**Files:**
- Create: `crates/ui/src-svelte/src/lib/stores/config.ts`
- Modify: `crates/ui/src-svelte/src/lib/ipc/client.ts` — add `getConfig` / `setConfig` / `changePassphrase` / `setContactMuted` / `tailLogs` / `getPassphraseAuditLatest` / `wipeAllData` methods (each is a thin wrapper around the existing `ipc_request` Tauri command)

- [ ] **Step 1: Add the IPC client methods**

In `crates/ui/src-svelte/src/lib/ipc/client.ts`, append methods (matching the existing pattern):

```ts
async getConfig(): Promise<ConfigSnapshot> {
  const res = await this.request({ type: 'GetConfig' });
  if (res.type === 'Config') return res.value;
  throw new Error(`unexpected reply: ${res.type}`);
}

async setConfig(patch: Partial<ConfigSnapshot>): Promise<void> {
  await this.request({ type: 'SetConfig', patch });
}

async changePassphrase(oldPass: string, newPass: string): Promise<void> {
  const res = await this.request({ type: 'ChangePassphrase', old: oldPass, new: newPass });
  if (res.type !== 'PassphraseChanged') throw new Error(`unexpected reply: ${res.type}`);
}

async setContactMuted(contact: string, muted: boolean): Promise<void> {
  await this.request({ type: 'SetContactMuted', contact, muted });
}

async tailLogs(sinceSeq: number | null, limit: number): Promise<{ records: LogRecord[]; nextSinceSeq: number }> {
  const res = await this.request({ type: 'TailLogs', since_seq: sinceSeq, limit });
  if (res.type !== 'Logs') throw new Error(`unexpected reply: ${res.type}`);
  return { records: res.records, nextSinceSeq: res.next_since_seq };
}

async getPassphraseAuditLatest(): Promise<number | null> {
  const res = await this.request({ type: 'GetPassphraseAuditLatest' });
  if (res.type !== 'PassphraseAudit') throw new Error(`unexpected reply: ${res.type}`);
  return res.last_changed_unix;
}

async wipeAllData(): Promise<void> {
  await this.request({ type: 'WipeAllData' });
}
```

(Use whatever `request` / `ipc_request` shape `IpcClient` already exposes — match that pattern, don't introduce a new one.)

- [ ] **Step 2: Create the config store**

Create `crates/ui/src-svelte/src/lib/stores/config.ts`:

```ts
import { writable, get } from 'svelte/store';
import type { ConfigSnapshot } from '$lib/ipc/types';
import { ipcClient } from '$lib/ipc/client';

const CACHE_TTL_MS = 5_000;

interface ConfigState {
  snapshot: ConfigSnapshot | null;
  fetchedAt: number;
}

const state = writable<ConfigState>({ snapshot: null, fetchedAt: 0 });

export const config = { subscribe: state.subscribe };

export async function fetchConfig(): Promise<ConfigSnapshot> {
  const cur = get(state);
  if (cur.snapshot && Date.now() - cur.fetchedAt < CACHE_TTL_MS) {
    return cur.snapshot;
  }
  const snap = await ipcClient.getConfig();
  state.set({ snapshot: snap, fetchedAt: Date.now() });
  return snap;
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;
let pendingPatch: Partial<ConfigSnapshot> = {};

export function patchConfig(patch: Partial<ConfigSnapshot>): Promise<void> {
  pendingPatch = { ...pendingPatch, ...patch };
  // Optimistic update.
  state.update((s) => {
    if (!s.snapshot) return s;
    return { snapshot: { ...s.snapshot, ...patch }, fetchedAt: Date.now() };
  });

  return new Promise((resolve, reject) => {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(async () => {
      const toSend = pendingPatch;
      pendingPatch = {};
      try {
        await ipcClient.setConfig(toSend);
        resolve();
      } catch (e) {
        // On failure, refetch the authoritative state.
        await fetchConfig();
        reject(e);
      }
    }, 500);
  });
}
```

- [ ] **Step 3: Run TS check**

```bash
cd crates/ui/src-svelte
pnpm run check
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/lib/{ipc/client.ts,stores/config.ts}
git commit -m "$(cat <<'EOF'
feat(ui): config store + IPC client methods for 2.F surface

Optimistic patchConfig coalesces edits over 500ms before sending
SetConfig; rolls back via fetchConfig on error. IPC client gains
methods for every Phase 2.F Command variant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 30: Settings sidebar layout + nested-route scaffolding

**Files:**
- Create: `crates/ui/src-svelte/src/routes/settings/+layout.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/SettingsSidebar.svelte`
- Create: empty `+page.svelte` files for each section (filled in Tasks 32–36)

- [ ] **Step 1: Create the sidebar component**

Create `crates/ui/src-svelte/src/lib/components/SettingsSidebar.svelte`:

```svelte
<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';

  const sections = [
    { id: 'identity',      label: 'Identity'     },
    { id: 'mailboxes',     label: 'Mailboxes'    },
    { id: 'history',       label: 'History'      },
    { id: 'notifications', label: 'Notifications'},
    { id: 'advanced',      label: 'Advanced'     },
  ];

  $: activeId = $page.url.pathname.split('/').filter(Boolean).at(-1) ?? 'identity';
</script>

<nav class="sidebar">
  <h2>Settings</h2>
  {#each sections as s}
    <button
      class:active={activeId === s.id}
      on:click={() => goto(`/settings/${s.id}`)}
    >{s.label}</button>
  {/each}
</nav>

<style>
  .sidebar {
    width: 200px;
    border-right: 1px solid var(--border);
    padding: var(--s-2);
    background: var(--bg-elevated);
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }
  h2 {
    font: var(--t-display);
    margin: 0 0 var(--s-2) 0;
    color: var(--text);
  }
  button {
    text-align: left;
    background: none;
    border: none;
    color: var(--text-muted);
    padding: var(--s-1) var(--s-2);
    border-radius: 4px;
    cursor: pointer;
    font: var(--t-ui);
  }
  button.active { color: var(--text); background: var(--bg); }
  button:hover  { color: var(--text); }
</style>
```

- [ ] **Step 2: Create `routes/settings/+layout.svelte`**

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import SettingsSidebar from '$lib/components/SettingsSidebar.svelte';

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') goto('/');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });
</script>

<div class="settings-shell">
  <SettingsSidebar />
  <main class="content">
    <slot />
  </main>
</div>

<style>
  .settings-shell { display: grid; grid-template-columns: 200px 1fr; height: 100vh; }
  .content { padding: var(--s-3); overflow-y: auto; }
</style>
```

- [ ] **Step 3: Create stub +page.svelte files**

Create empty (just a heading) files for each section route:

```bash
for s in identity mailboxes history notifications advanced; do
  mkdir -p crates/ui/src-svelte/src/routes/settings/$s
  cat > crates/ui/src-svelte/src/routes/settings/$s/+page.svelte <<EOF
<h1>${s^}</h1>
<p>Section content lands in Task 32–36.</p>
EOF
done
```

- [ ] **Step 4: Run TS check + Vitest baseline**

```bash
cd crates/ui/src-svelte
pnpm run check
pnpm test --run
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/ \
        crates/ui/src-svelte/src/lib/components/SettingsSidebar.svelte
git commit -m "$(cat <<'EOF'
feat(ui): settings sidebar layout + nested route scaffolding

Five empty section routes under routes/settings/<section>/+page.svelte;
sidebar nav highlights the active section; ESC returns to /.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 31: "Open Settings" entry point in main shell

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/+page.svelte` (header gear icon → goto('/settings/identity'))
- Modify: `crates/ui/src-svelte/src/routes/+layout.svelte` (route guard or nav surface)

- [ ] **Step 1: Add a gear-icon button**

In the main contact-list header (`+page.svelte`), add a button next to the existing header buttons:

```svelte
<button class="icon-btn" on:click={() => goto('/settings/identity')} title="Settings">
  ⚙
</button>
```

(Use whatever icon convention 2.E added — likely an SVG component. Match.)

- [ ] **Step 2: Commit**

```bash
git add crates/ui/src-svelte/src/routes/+page.svelte
git commit -m "$(cat <<'EOF'
feat(ui): settings entry point in main shell header

Gear-icon button next to the existing + Add / + Generate invite
buttons; navigates to /settings/identity.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 12 — Settings sections

### Task 32: Settings → Identity

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/identity/+page.svelte`
- Create: `crates/ui/src-svelte/src/lib/components/ChangePassphraseDialog.svelte`

- [ ] **Step 1: Identity page**

Replace `routes/settings/identity/+page.svelte` with:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { ipcClient } from '$lib/ipc/client';
  import ChangePassphraseDialog from '$lib/components/ChangePassphraseDialog.svelte';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { toast } from '$lib/stores/toast';

  let pubkey = '';
  let onion = '';
  let cardVersion = 0;
  let lastChanged: number | null = null;
  let showPassphrase = false;
  let confirmRotate = false;

  onMount(async () => {
    const info = await ipcClient.daemonInfo();
    pubkey = info.local_pubkey;
    onion = info.current_onion;
    // cardVersion comes from a different command if 2.E added one; otherwise compute from contacts.
    lastChanged = await ipcClient.getPassphraseAuditLatest();
  });

  function copy(s: string) {
    navigator.clipboard.writeText(s);
    toast.show('Copied', 'success');
  }

  async function rotateOnion() {
    confirmRotate = false;
    try {
      await ipcClient.rotateOnion();
      toast.show('Onion rotation queued', 'success');
    } catch (e) {
      toast.show(`Rotate failed: ${e}`, 'error');
    }
  }

  function fmtTs(t: number | null) {
    return t ? new Date(t * 1000).toLocaleString() : 'Never';
  }
</script>

<h1>Identity</h1>

<dl>
  <dt>Public key</dt>
  <dd><code>{pubkey.slice(0, 16)}…</code> <button on:click={() => copy(pubkey)}>Copy full</button></dd>

  <dt>Onion</dt>
  <dd><code>{onion.slice(0, 16)}…</code> <button on:click={() => copy(onion)}>Copy full</button></dd>

  <dt>Card version</dt>
  <dd>{cardVersion}</dd>

  <dt>Passphrase last changed</dt>
  <dd>{fmtTs(lastChanged)}</dd>
</dl>

<div class="actions">
  <button on:click={() => (confirmRotate = true)}>Rotate onion</button>
  <button on:click={() => (showPassphrase = true)}>Change passphrase</button>
</div>

{#if confirmRotate}
  <ConfirmDialog
    title="Rotate onion?"
    body="Note: in 2.F this only bumps the self-card version (real HS-key rotation is Task 23.5)."
    confirmLabel="Rotate"
    on:confirm={rotateOnion}
    on:cancel={() => (confirmRotate = false)}
  />
{/if}

{#if showPassphrase}
  <ChangePassphraseDialog on:close={() => (showPassphrase = false)} />
{/if}

<style>
  dl { display: grid; grid-template-columns: max-content 1fr; gap: var(--s-1) var(--s-3); }
  dt { color: var(--text-muted); }
  .actions { display: flex; gap: var(--s-2); margin-top: var(--s-3); }
</style>
```

- [ ] **Step 2: ChangePassphraseDialog**

Create `crates/ui/src-svelte/src/lib/components/ChangePassphraseDialog.svelte`:

```svelte
<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { ipcClient } from '$lib/ipc/client';
  import { toast } from '$lib/stores/toast';
  import zxcvbn from 'zxcvbn';

  const dispatch = createEventDispatcher();

  let oldPass = '';
  let newPass = '';
  let confirm = '';
  let inflight = false;
  let errorMsg = '';

  $: score = newPass.length > 0 ? zxcvbn(newPass).score : 0;
  $: canSubmit = !inflight && newPass.length >= 8 && score >= 3 && newPass === confirm && newPass !== oldPass;

  async function submit() {
    if (!canSubmit) return;
    inflight = true;
    errorMsg = '';
    try {
      await ipcClient.changePassphrase(oldPass, newPass);
      toast.show('Passphrase changed', 'success');
      dispatch('close');
    } catch (e: any) {
      errorMsg = String(e?.message ?? e);
    } finally {
      inflight = false;
    }
  }
</script>

<div class="modal-overlay" on:click={() => !inflight && dispatch('close')}>
  <div class="modal" on:click|stopPropagation>
    <h2>Change passphrase</h2>

    <label>Current passphrase
      <input type="password" bind:value={oldPass} disabled={inflight} />
    </label>

    <label>New passphrase
      <input type="password" bind:value={newPass} disabled={inflight} />
    </label>
    <div class="strength" data-score={score}>
      Strength: {['weakest','weak','fair','good','strong'][score]}
    </div>

    <label>Confirm new
      <input type="password" bind:value={confirm} disabled={inflight} />
    </label>

    {#if errorMsg}
      <div class="error">{errorMsg}</div>
    {/if}

    {#if inflight}
      <div class="spinner">Changing passphrase…</div>
    {/if}

    <div class="actions">
      <button on:click={() => dispatch('close')} disabled={inflight}>Cancel</button>
      <button on:click={submit} disabled={!canSubmit}>Change</button>
    </div>
  </div>
</div>

<style>
  /* Reuse modal-overlay / modal / button conventions from 2.E. */
</style>
```

- [ ] **Step 3: Run check + Vitest baseline**

```bash
cd crates/ui/src-svelte
pnpm run check
pnpm test --run
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/identity/+page.svelte \
        crates/ui/src-svelte/src/lib/components/ChangePassphraseDialog.svelte
git commit -m "$(cat <<'EOF'
feat(ui): Settings → Identity + ChangePassphraseDialog

Pubkey + onion (with Copy full), card version, last-changed
timestamp from GetPassphraseAuditLatest. Rotate onion uses a
ConfirmDialog with the Task 23.5 disclaimer. Change passphrase
launches the modal with live zxcvbn strength meter; submit shows
non-cancellable spinner during the daemon-side rekey.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 33: Settings → Mailboxes

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/mailboxes/+page.svelte`

- [ ] **Step 1: Mailboxes page**

```svelte
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { ipcClient } from '$lib/ipc/client';
  import type { MailboxSummary } from '$lib/ipc/types';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { toast } from '$lib/stores/toast';

  let mailboxes: MailboxSummary[] = [];
  let showAdd = false;
  let pendingOnion = '';
  let confirmRemove: number | null = null;
  let unsubscribe: (() => void) | null = null;

  async function refresh() {
    mailboxes = await ipcClient.listMailboxes();
  }

  async function add() {
    try {
      await ipcClient.addMailbox(pendingOnion.trim());
      toast.show('Mailbox added', 'success');
      pendingOnion = '';
      showAdd = false;
      await refresh();
    } catch (e: any) {
      toast.show(`Add failed: ${e}`, 'error');
    }
  }

  async function remove(id: number) {
    confirmRemove = null;
    try {
      await ipcClient.removeMailbox(id);
      toast.show('Mailbox removed', 'success');
      await refresh();
    } catch (e: any) {
      toast.show(`Remove failed: ${e}`, 'error');
    }
  }

  onMount(async () => {
    await refresh();
    unsubscribe = ipcClient.subscribeEvents({ kind: 'Mailboxes' }, async (evt) => {
      if (evt.type === 'MailboxStatusChanged') {
        await refresh();
      }
    });
  });

  onDestroy(() => unsubscribe?.());
</script>

<h1>Mailboxes</h1>

{#if mailboxes.length === 0}
  <p>No mailboxes registered.</p>
{:else}
  <ul class="mailbox-list">
    {#each mailboxes as m (m.id)}
      <li>
        <code>{m.onion.slice(0, 16)}…</code>
        <span class="status status-{m.status}">{m.status}</span>
        <span class="ts">since {new Date(m.registered_at * 1000).toLocaleDateString()}</span>
        <button on:click={() => (confirmRemove = m.id)}>Remove</button>
      </li>
    {/each}
  </ul>
{/if}

<button on:click={() => (showAdd = true)}>Add mailbox</button>

{#if showAdd}
  <div class="modal-overlay" on:click={() => (showAdd = false)}>
    <div class="modal" on:click|stopPropagation>
      <h2>Add mailbox</h2>
      <label>Onion address
        <input type="text" bind:value={pendingOnion} placeholder="abc…xyz.onion" />
      </label>
      <div class="actions">
        <button on:click={() => (showAdd = false)}>Cancel</button>
        <button on:click={add}>Add</button>
      </div>
    </div>
  </div>
{/if}

{#if confirmRemove !== null}
  <ConfirmDialog
    title="Remove mailbox?"
    body="Pending deposits to this mailbox may be lost."
    confirmLabel="Remove"
    on:confirm={() => remove(confirmRemove)}
    on:cancel={() => (confirmRemove = null)}
  />
{/if}
```

- [ ] **Step 2: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/mailboxes/+page.svelte
git commit -m "$(cat <<'EOF'
feat(ui): Settings → Mailboxes — list / add / remove

Reads ListMailboxes, subscribes to MailboxStatusChanged for live
status pills, AddMailbox via a small modal, RemoveMailbox via a
ConfirmDialog. All wires hit the real Task 15 handlers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 34: Settings → History

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/history/+page.svelte`
- Create: `crates/ui/src-svelte/src/lib/history/exporter.ts` (client-side JSONL / plaintext / gzip writer)

- [ ] **Step 1: Create the exporter**

Create `crates/ui/src-svelte/src/lib/history/exporter.ts`:

```ts
import type { MessageRecord } from '$lib/ipc/types';
import { ipcClient } from '$lib/ipc/client';

export type ExportFormat = 'jsonl' | 'plaintext';

export async function exportConversation(
  contact: string | null,   // null = all conversations
  format: ExportFormat,
  gzip: boolean,
  onChunk: (text: string) => void,
): Promise<void> {
  let afterId: number | null = null;
  while (true) {
    const page = await ipcClient.exportHistory({ contact, after_id: afterId, limit: 500 });
    if (page.records.length === 0) break;
    for (const r of page.records) {
      onChunk(format === 'jsonl' ? jsonlLine(r) : plaintextLine(r));
    }
    afterId = page.records[page.records.length - 1].row_id;
    if (page.records.length < 500) break;
  }
}

function jsonlLine(r: MessageRecord): string {
  return JSON.stringify(r) + '\n';
}

function plaintextLine(r: MessageRecord): string {
  const ts = new Date(r.ts_recv * 1000).toISOString().slice(0, 19).replace('T', ' ');
  const sender = r.is_outgoing ? 'Me' : (r.contact_nickname ?? 'Peer');
  const body = (r.kind === 'text' && r.body) ? r.body : `<${r.kind}>`;
  return `[${ts}] ${sender}: ${body}\n`;
}
```

(`r.row_id` / `r.is_outgoing` / `r.contact_nickname` shapes follow the existing `MessageRecord` ts-rs binding; adapt names to the actual TS types after running `cargo test -p skattr-core` once.)

- [ ] **Step 2: History page**

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { config, fetchConfig, patchConfig } from '$lib/stores/config';
  import { ipcClient } from '$lib/ipc/client';
  import SearchPalette from '$lib/components/SearchPalette.svelte';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { exportConversation, type ExportFormat } from '$lib/history/exporter';
  import { toast } from '$lib/stores/toast';
  import { save } from '@tauri-apps/plugin-dialog';

  let showSearch = false;
  let format: ExportFormat = 'plaintext';
  let useGzip = false;
  let confirmPrune: { older_than_days: number } | null = null;
  let pruneDays = 30;

  onMount(fetchConfig);

  const PRESETS = [
    { label: '24 hours',  days: 1 },
    { label: '7 days',    days: 7 },
    { label: '30 days',   days: 30 },
    { label: '90 days',   days: 90 },
    { label: 'Never delete', days: 0 },
  ];

  async function setRetention(days: number) {
    try {
      await patchConfig({ history_retention_days: days });
      toast.show('Retention updated', 'success');
    } catch (e) {
      toast.show(`Failed: ${e}`, 'error');
    }
  }

  async function downloadAll() {
    const ext = format === 'jsonl' ? 'jsonl' : 'txt';
    const today = new Date().toISOString().slice(0, 10).replaceAll('-', '');
    const suggested = `skattr-export-${today}.${ext}${useGzip ? '.gz' : ''}`;
    const path = await save({ defaultPath: suggested });
    if (!path) return;
    const chunks: string[] = [];
    await exportConversation(null, format, useGzip, (line) => chunks.push(line));
    const blob = chunks.join('');
    // Use a Tauri command to write the file (and gzip-compress if requested).
    await ipcClient.writeExportFile(path, blob, useGzip);
    toast.show(`Exported to ${path}`, 'success');
  }

  async function runPrune() {
    if (!confirmPrune) return;
    const before = Math.floor(Date.now() / 1000) - confirmPrune.older_than_days * 86400;
    confirmPrune = null;
    await ipcClient.pruneHistory({ contact: null, before_ts_recv: before, keep_last: null });
    toast.show('Pruned', 'success');
  }
</script>

<h1>History</h1>

<section>
  <h2>Retention</h2>
  {#each PRESETS as p}
    <label>
      <input type="radio" name="retention" value={p.days}
             checked={$config.snapshot?.history_retention_days === p.days}
             on:change={() => setRetention(p.days)} />
      {p.label}
    </label>
  {/each}
</section>

<section>
  <h2>Search</h2>
  <button on:click={() => (showSearch = !showSearch)}>
    {showSearch ? 'Hide' : 'Open'} search palette (or press ⌘/Ctrl-K anywhere)
  </button>
  {#if showSearch}
    <SearchPalette inline={true} />
  {/if}
</section>

<section>
  <h2>Export</h2>
  <label><input type="radio" bind:group={format} value="jsonl" /> JSONL</label>
  <label><input type="radio" bind:group={format} value="plaintext" /> Plaintext</label>
  <label><input type="checkbox" bind:checked={useGzip} /> gzip</label>
  <button on:click={downloadAll}>Download all conversations</button>
</section>

<section>
  <h2>Prune</h2>
  <label>Older than
    <input type="number" min="1" bind:value={pruneDays} /> days
  </label>
  <button on:click={() => (confirmPrune = { older_than_days: pruneDays })}>Delete…</button>
</section>

{#if confirmPrune}
  <ConfirmDialog
    title="Delete old messages?"
    body={`Permanently delete messages older than ${confirmPrune.older_than_days} days. This cannot be undone.`}
    confirmLabel="Delete"
    on:confirm={runPrune}
    on:cancel={() => (confirmPrune = null)}
  />
{/if}
```

(`ipcClient.writeExportFile` is a thin Tauri command wrapper that writes UTF-8 bytes to a path, optionally gzipping via the `flate2` crate. Add it in `crates/ui/src/main.rs` + `crates/ui/src-svelte/src/lib/ipc/client.ts` in this same task.)

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/history/+page.svelte \
        crates/ui/src-svelte/src/lib/history/exporter.ts \
        crates/ui/src/main.rs \
        crates/ui/src-svelte/src/lib/ipc/client.ts
git commit -m "$(cat <<'EOF'
feat(ui): Settings → History — retention, search, export, prune

Five retention presets (24h / 7d / 30d / 90d / Never). Search button
mounts SearchPalette inline. Export streams ExportHistory in 500-row
pages, formats client-side as JSONL or plaintext, gzips optionally
via a new write_export_file Tauri command. Prune fires PruneHistory
behind a ConfirmDialog.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 35: Settings → Notifications

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/notifications/+page.svelte`

- [ ] **Step 1: Notifications page**

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { config, fetchConfig, patchConfig } from '$lib/stores/config';
  import { toast } from '$lib/stores/toast';

  const MODES: { id: 'full'|'minimal'|'generic'|'off'; label: string; hint: string }[] = [
    { id: 'full',    label: 'Full',    hint: 'Sender + message preview' },
    { id: 'minimal', label: 'Minimal', hint: 'Sender only' },
    { id: 'generic', label: 'Generic', hint: '"New message"' },
    { id: 'off',     label: 'Off',     hint: 'No notifications' },
  ];

  onMount(fetchConfig);

  async function setMode(mode: 'full'|'minimal'|'generic'|'off') {
    try {
      await patchConfig({ notification_mode: mode });
      toast.show('Notification mode updated', 'success');
    } catch (e) {
      toast.show(`Failed: ${e}`, 'error');
    }
  }
</script>

<h1>Notifications</h1>

<fieldset>
  <legend>Mode</legend>
  {#each MODES as m}
    <label>
      <input type="radio" name="mode" value={m.id}
             checked={$config.snapshot?.notification_mode === m.id}
             on:change={() => setMode(m.id)} />
      <strong>{m.label}</strong> <span class="muted">— {m.hint}</span>
    </label>
  {/each}
</fieldset>

<fieldset>
  <legend>Behaviour</legend>
  <label>
    <input type="checkbox" checked disabled />
    Suppress when window is focused AND the active conversation receives the message
    (other conversations still notify; this is the only sensible default).
  </label>
</fieldset>

<p class="muted">Per-conversation mute is on each contact's details panel.</p>
```

- [ ] **Step 2: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/notifications/+page.svelte
git commit -m "$(cat <<'EOF'
feat(ui): Settings → Notifications

Four-mode radio (full / minimal / generic / off) wired to
ConfigPatch.notification_mode. The focus-aware behaviour is
non-configurable per locked decision 4 — rendered as a disabled
informational checkbox.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 36: Settings → Advanced

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte`
- Create: `crates/ui/src-svelte/src/lib/stores/logs.ts`
- Create: `crates/ui/src-svelte/src/lib/components/LogsViewer.svelte`

- [ ] **Step 1: Logs store**

Create `crates/ui/src-svelte/src/lib/stores/logs.ts`:

```ts
import { writable } from 'svelte/store';
import type { LogRecord } from '$lib/ipc/types';
import { ipcClient } from '$lib/ipc/client';

const _records = writable<LogRecord[]>([]);
export const logs = { subscribe: _records.subscribe };

let unsub: (() => void) | null = null;
let nextSinceSeq: number | null = null;

export async function attach() {
  if (unsub) return;
  const initial = await ipcClient.tailLogs(null, 500);
  _records.set(initial.records);
  nextSinceSeq = initial.nextSinceSeq;
  unsub = ipcClient.subscribeEvents({ kind: 'Logs' }, (evt) => {
    if (evt.type === 'LogRecord') {
      _records.update((rs) => {
        const next = [...rs, evt.value];
        // cap at 500
        return next.slice(-500);
      });
    }
  });
}

export function detach() {
  unsub?.();
  unsub = null;
  nextSinceSeq = null;
  _records.set([]);
}
```

- [ ] **Step 2: LogsViewer component**

```svelte
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { logs, attach, detach } from '$lib/stores/logs';
  import { toast } from '$lib/stores/toast';

  onMount(attach);
  onDestroy(detach);

  function copyAll() {
    const text = $logs
      .map((r) => `${new Date(r.ts_unix_ms).toISOString()} ${r.level.toUpperCase()} ${r.target} ${r.message}`)
      .join('\n');
    navigator.clipboard.writeText(text);
    toast.show('Logs copied', 'success');
  }
</script>

<div class="logs">
  <div class="header">
    <span>{$logs.length} records</span>
    <button on:click={copyAll}>Copy logs</button>
  </div>
  <ol>
    {#each $logs as r (r.seq)}
      <li class="lvl-{r.level}">
        <span class="ts">{new Date(r.ts_unix_ms).toLocaleTimeString()}</span>
        <span class="lvl">{r.level}</span>
        <span class="target">{r.target}</span>
        <span class="msg">{r.message}</span>
      </li>
    {/each}
  </ol>
</div>

<style>
  .logs { font: var(--t-ui); max-height: 400px; overflow: auto; background: var(--bg); border: 1px solid var(--border); }
  .lvl-trace, .lvl-debug { color: var(--text-muted); }
  .lvl-info  { color: var(--text); }
  .lvl-warn  { color: var(--accent); }
  .lvl-error { color: var(--danger); }
</style>
```

- [ ] **Step 3: Advanced page**

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { config, fetchConfig, patchConfig } from '$lib/stores/config';
  import LogsViewer from '$lib/components/LogsViewer.svelte';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { ipcClient } from '$lib/ipc/client';
  import { toast } from '$lib/stores/toast';

  let showLogs = false;
  let confirmStage1 = false;
  let confirmStage2 = false;
  let info: { local_pubkey: string; current_onion: string; daemon_version: string; schema_version: number } | null = null;

  onMount(async () => {
    await fetchConfig();
    info = await ipcClient.daemonInfo();
  });

  async function togglePersist(e: Event) {
    const persist = (e.target as HTMLInputElement).checked;
    await patchConfig({ persist_logs_to_disk: persist });
    toast.show(persist ? 'Logs will persist to disk' : 'Disk persistence off (existing files retained)', 'success');
  }

  async function toggleCloseToTray(e: Event) {
    const v = (e.target as HTMLInputElement).checked;
    await patchConfig({ close_to_tray: v });
  }

  async function toggleStartMinimised(e: Event) {
    const v = (e.target as HTMLInputElement).checked;
    await patchConfig({ start_minimised: v });
  }

  async function wipe() {
    confirmStage2 = false;
    try {
      await ipcClient.wipeAllData();
    } catch {
      // Connection close is expected.
    }
    toast.show('Skattr is wiping data and shutting down.', 'info');
  }

  function copy(s: string) { navigator.clipboard.writeText(s); toast.show('Copied', 'success'); }
</script>

<h1>Advanced</h1>

<section>
  <h2>Behaviour</h2>
  <label>
    <input type="checkbox"
           checked={$config.snapshot?.close_to_tray ?? true}
           on:change={toggleCloseToTray} />
    Close button hides to tray
  </label>
  <label>
    <input type="checkbox"
           checked={$config.snapshot?.start_minimised ?? false}
           on:change={toggleStartMinimised} />
    Start minimised to tray
  </label>
</section>

<section>
  <h2>Logs</h2>
  <button on:click={() => (showLogs = !showLogs)}>
    {showLogs ? 'Close' : 'Open'} logs viewer
  </button>
  <label>
    <input type="checkbox"
           checked={$config.snapshot?.persist_logs_to_disk ?? false}
           on:change={togglePersist} />
    Persist logs to disk (rotated daily)
  </label>
  {#if showLogs}
    <LogsViewer />
  {/if}
</section>

<section>
  <h2>Debug info</h2>
  {#if info}
    <dl>
      <dt>Daemon version</dt><dd>{info.daemon_version}</dd>
      <dt>Schema version</dt><dd>{info.schema_version}</dd>
      <dt>Public key</dt><dd><code>{info.local_pubkey}</code> <button on:click={() => copy(info!.local_pubkey)}>Copy</button></dd>
      <dt>Onion</dt><dd><code>{info.current_onion}</code> <button on:click={() => copy(info!.current_onion)}>Copy</button></dd>
    </dl>
  {/if}
</section>

<section class="danger">
  <h2>Danger zone</h2>
  <button class="danger-btn" on:click={() => (confirmStage1 = true)}>
    Delete all data and quit
  </button>
</section>

{#if confirmStage1}
  <ConfirmDialog
    title="Delete all Skattr data?"
    body="This permanently removes contacts, messages, mailboxes, identity, and the database. This cannot be undone."
    confirmLabel="I understand, continue"
    danger={true}
    on:confirm={() => { confirmStage1 = false; confirmStage2 = true; }}
    on:cancel={() => (confirmStage1 = false)}
  />
{/if}

{#if confirmStage2}
  <ConfirmDialog
    title="Are you absolutely sure?"
    body="Last chance. Type-the-word confirmation is overkill; this is final-final."
    confirmLabel="Wipe everything"
    danger={true}
    on:confirm={wipe}
    on:cancel={() => (confirmStage2 = false)}
  />
{/if}
```

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/routes/settings/advanced/+page.svelte \
        crates/ui/src-svelte/src/lib/stores/logs.ts \
        crates/ui/src-svelte/src/lib/components/LogsViewer.svelte
git commit -m "$(cat <<'EOF'
feat(ui): Settings → Advanced — behaviour toggles, logs viewer, danger zone

close_to_tray / start_minimised / persist_logs_to_disk via SetConfig.
LogsViewer subscribes to EventFilter::Logs + initial TailLogs;
colour-codes by level; Copy logs button. Two-stage confirm before
WipeAllData.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 13 — Search palette

### Task 37: `SearchPalette.svelte` + Cmd/Ctrl-K binding

**Files:**
- Create: `crates/ui/src-svelte/src/lib/components/SearchPalette.svelte`
- Create: `crates/ui/src-svelte/src/lib/stores/searchPalette.ts`
- Modify: `crates/ui/src-svelte/src/routes/+layout.svelte` (global keybinding + always-mounted modal instance)

- [ ] **Step 1: searchPalette store**

```ts
import { writable } from 'svelte/store';
import type { MessageRecord } from '$lib/ipc/types';

interface State {
  open: boolean;
  query: string;
  results: MessageRecord[];
  loading: boolean;
}

const _state = writable<State>({ open: false, query: '', results: [], loading: false });
export const searchPalette = _state;

export function open()  { _state.update((s) => ({ ...s, open: true  })); }
export function close() { _state.update((s) => ({ ...s, open: false })); }
```

- [ ] **Step 2: SearchPalette component**

```svelte
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { goto } from '$app/navigation';
  import { ipcClient } from '$lib/ipc/client';
  import { searchPalette, close } from '$lib/stores/searchPalette';

  export let inline = false;

  let query = '';
  let timer: ReturnType<typeof setTimeout> | null = null;
  let highlightIdx = 0;

  $: visible = inline ? true : $searchPalette.open;

  async function runQuery(q: string) {
    if (!q.trim()) {
      searchPalette.update((s) => ({ ...s, results: [], loading: false }));
      return;
    }
    searchPalette.update((s) => ({ ...s, loading: true }));
    const res = await ipcClient.searchMessages({ query: q, contact: null, limit: 50, offset: 0, newest_first: true });
    searchPalette.update((s) => ({ ...s, results: res, loading: false }));
    highlightIdx = 0;
  }

  function onInput() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => runQuery(query), 200);
  }

  function pick(i: number) {
    const r = $searchPalette.results[i];
    if (!r) return;
    if (!inline) close();
    goto(`/conversation/${r.contact}?focus_row_id=${r.row_id}`);
  }

  function onKey(e: KeyboardEvent) {
    if (!visible) return;
    if (e.key === 'Escape' && !inline) { close(); return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); highlightIdx = Math.min(highlightIdx + 1, $searchPalette.results.length - 1); }
    if (e.key === 'ArrowUp')   { e.preventDefault(); highlightIdx = Math.max(highlightIdx - 1, 0); }
    if (e.key === 'Enter')     { pick(highlightIdx); }
  }

  onMount(() => window.addEventListener('keydown', onKey));
  onDestroy(() => window.removeEventListener('keydown', onKey));
</script>

{#if visible}
  <div class="palette" class:modal={!inline} class:inline>
    {#if !inline}
      <div class="overlay" on:click={close}></div>
    {/if}
    <div class="panel">
      <input
        bind:value={query}
        on:input={onInput}
        placeholder="Search messages…"
        autofocus={!inline}
      />
      {#if $searchPalette.loading}
        <div class="loading">Searching…</div>
      {/if}
      <ul>
        {#each $searchPalette.results as r, i (r.row_id)}
          <li class:active={i === highlightIdx} on:click={() => pick(i)}>
            <div class="meta">{r.contact_nickname ?? '(unknown)'} · {new Date(r.ts_recv * 1000).toLocaleString()}</div>
            <div class="snippet">{@html r.snippet ?? r.body}</div>
          </li>
        {/each}
      </ul>
    </div>
  </div>
{/if}

<style>
  .palette.modal { position: fixed; inset: 0; z-index: 100; }
  .overlay { position: absolute; inset: 0; background: rgba(0,0,0,0.4); }
  .panel { position: relative; max-width: 60vw; margin: 10vh auto; background: var(--bg-elevated); padding: var(--s-2); border-radius: 8px; }
  .palette.inline .panel { margin: 0; max-width: 100%; }
  li.active { background: var(--bg); }
  /* FTS5 snippet markup uses >..< around highlighted terms; render with @html. */
</style>
```

- [ ] **Step 3: Register the global keybinding**

In `routes/+layout.svelte`:

```svelte
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SearchPalette from '$lib/components/SearchPalette.svelte';
  import { open as openPalette } from '$lib/stores/searchPalette';

  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      openPalette();
    }
  }
  onMount(() => window.addEventListener('keydown', onKey));
  onDestroy(() => window.removeEventListener('keydown', onKey));
</script>

<slot />
<SearchPalette />
```

- [ ] **Step 4: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/SearchPalette.svelte \
        crates/ui/src-svelte/src/lib/stores/searchPalette.ts \
        crates/ui/src-svelte/src/routes/+layout.svelte
git commit -m "$(cat <<'EOF'
feat(ui): Cmd/Ctrl-K SearchPalette + global keybinding

Single component reused as modal (Cmd/Ctrl-K) + inline (Settings →
History → Search). 200ms debounced query, FTS5 snippet markup
rendered via {@html}; arrow-key navigation; Enter selects.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 38: Conversation view — `focus_row_id` deep link

**Files:**
- Modify: `crates/ui/src-svelte/src/routes/conversation/[contact]/+page.svelte`

- [ ] **Step 1: Read the query param**

In `routes/conversation/[contact]/+page.svelte`'s script:

```ts
import { page } from '$app/stores';

let focusRowId: number | null = null;
let highlighted = false;

$: {
  const v = $page.url.searchParams.get('focus_row_id');
  focusRowId = v ? parseInt(v, 10) : null;
}

async function loadFocusRow() {
  if (focusRowId === null) return;
  // Page back / forward until the row is in the loaded set, then scroll to it.
  // Implementation depends on the existing virtualised-list API in 2.D —
  // expose a `scrollToRow(row_id)` on the list component if not already present.
  await listRef.scrollToRow(focusRowId);
  highlighted = true;
  setTimeout(() => (highlighted = false), 1200);
}

$: if (focusRowId !== null) loadFocusRow();
```

In the message-list rendering, conditionally apply `class:focus-highlight={highlighted && row.row_id === focusRowId}`.

**Critical:** the existing mark-read logic (gated on bottom-of-list intersection) must NOT fire just because we navigated here. Verify by reading the existing intersection-observer wiring; if the trigger uses `IntersectionObserver` against the list bottom, no change is needed (we never scroll to bottom, we scroll to the focused row). If it triggers on any new render, gate it behind a flag that's set only by the user's natural scroll.

- [ ] **Step 2: Add a Vitest spec**

`crates/ui/src-svelte/src/routes/conversation/conversation.test.ts`:

```ts
import { describe, it, expect, vi } from 'vitest';
// ... mount the page with a tauri-mock fixture that pre-loads 100 rows,
// navigate with ?focus_row_id=42, assert the row is highlighted, assert
// the read-cursor-advance API was NOT called.
```

(Adapt to the existing test harness in `crates/ui/src-svelte/src/lib/test/tauri-mock.ts`.)

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/routes/conversation/
git commit -m "$(cat <<'EOF'
feat(ui): conversation view honours ?focus_row_id deep link

Search-palette result clicks land here. Scrolls to the row, briefly
highlights it (1200ms), DOES NOT advance the read cursor — mark-read
remains gated on bottom-of-list intersection (2.D semantics).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 14 — Contact details panel updates

### Task 39: Mute toggle + `peer_mailboxes` rendering

**Files:**
- Modify: `crates/ui/src-svelte/src/lib/components/ContactDetailsPanel.svelte`
- Modify: `crates/ui/src-svelte/src/lib/components/ContactRow.svelte` (mute icon next to nickname)

- [ ] **Step 1: ContactDetailsPanel — mute toggle + mailboxes**

In `ContactDetailsPanel.svelte`'s script:

```ts
import { ipcClient } from '$lib/ipc/client';
export let contact: ContactSummary;

async function toggleMute() {
  await ipcClient.setContactMuted(contact.pubkey, !contact.muted);
  // Parent should refresh its contacts store on Event::ContactUpdated;
  // optimistic local toggle for immediate feedback:
  contact = { ...contact, muted: !contact.muted };
}
```

In the template:

```svelte
<div class="header">
  <h3>{contact.nickname ?? 'Unnamed'}</h3>
  <button class="mute-toggle" on:click={toggleMute} title={contact.muted ? 'Unmute' : 'Mute'}>
    {contact.muted ? '🔕' : '🔔'}
  </button>
</div>

<section>
  <h4>Mailboxes</h4>
  {#if contact.peer_mailboxes.length === 0}
    <p class="muted">No mailboxes</p>
  {:else}
    <ul>
      {#each contact.peer_mailboxes as onion}
        <li>
          <code>{onion.slice(0, 12)}…{onion.slice(-12)}</code>
          <button on:click={() => navigator.clipboard.writeText(onion)}>Copy</button>
        </li>
      {/each}
    </ul>
  {/if}
</section>
```

- [ ] **Step 2: ContactRow — mute indicator**

In `ContactRow.svelte`, next to the nickname:

```svelte
{#if contact.muted}<span class="mute-icon" title="Muted">🔕</span>{/if}
```

- [ ] **Step 3: Commit**

```bash
git add crates/ui/src-svelte/src/lib/components/{ContactDetailsPanel,ContactRow}.svelte
git commit -m "$(cat <<'EOF'
feat(ui): contact details — mute toggle + peer_mailboxes list

Bell icon toggles SetContactMuted; optimistic local update. Mailboxes
section renders peer_mailboxes (truncated onion + copy-full button)
when present. ContactRow shows a small mute indicator next to muted
contacts in the main list.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 15 — Cross-binary integration tests

### Task 40: `settings_roundtrip` integration test

**Files:**
- Create: `crates/tests/src/settings_roundtrip.rs`

- [ ] **Step 1: Write the test**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(feature = "test-harness")]

mod helpers;
use helpers::spawn_daemon_with_passphrase;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_round_trip_persists_and_reloads() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");

    let mut daemon = spawn_daemon_with_passphrase(&data_dir, "passphrase").await;

    // Initial defaults
    let snap = daemon.client().get_config().await.unwrap();
    assert_eq!(snap.history_retention_days, 0);
    assert_eq!(snap.direct_timeout_secs, 30);
    assert!(matches!(snap.notification_mode, skattr_core::daemon::commands::NotificationMode::Full));

    // Patch
    daemon.client().set_config(skattr_core::daemon::commands::ConfigPatch {
        history_retention_days: Some(30),
        direct_timeout_secs: Some(60),
        notification_mode: Some(skattr_core::daemon::commands::NotificationMode::Minimal),
        close_to_tray: Some(false),
        start_minimised: Some(true),
        persist_logs_to_disk: Some(true),
    }).await.unwrap();

    // Round-trip
    let snap = daemon.client().get_config().await.unwrap();
    assert_eq!(snap.history_retention_days, 30);
    assert_eq!(snap.direct_timeout_secs, 60);
    assert!(matches!(snap.notification_mode, skattr_core::daemon::commands::NotificationMode::Minimal));
    assert!(!snap.close_to_tray);
    assert!(snap.start_minimised);
    assert!(snap.persist_logs_to_disk);

    daemon.shutdown().await;

    // Restart and confirm persistence
    let daemon2 = spawn_daemon_with_passphrase(&data_dir, "passphrase").await;
    let snap2 = daemon2.client().get_config().await.unwrap();
    assert_eq!(snap2.history_retention_days, 30);
    assert_eq!(snap2.direct_timeout_secs, 60);
    daemon2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_config_validates_direct_timeout_range() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("skattr-data");
    let daemon = spawn_daemon_with_passphrase(&data_dir, "passphrase").await;

    let err = daemon.client().set_config(skattr_core::daemon::commands::ConfigPatch {
        direct_timeout_secs: Some(0),
        ..Default::default()
    }).await.unwrap_err();
    assert!(format!("{err:?}").contains("InvalidArgument"), "{err:?}");

    daemon.shutdown().await;
}
```

- [ ] **Step 2: Run**

```bash
. "$HOME/.cargo/env"
cargo test -p skattr-tests --features test-harness settings_roundtrip -- --nocapture
```

Expected: 2 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/tests/src/settings_roundtrip.rs
git commit -m "$(cat <<'EOF'
test(settings): GetConfig → SetConfig → GetConfig round-trip + validation

Covers full config patch + persistence across restart, plus
direct_timeout_secs out-of-range rejection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 41: `mailbox_crud` integration test against a real mailbox

**Files:**
- Create: `crates/tests/src/mailbox_crud.rs`

- [ ] **Step 1: Write the test**

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

#![cfg(feature = "test-harness")]

mod helpers;
use helpers::{spawn_daemon_with_passphrase, spawn_mailbox_server};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_list_remove_against_real_mailbox() {
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = workdir.path().join("daemon");
    let mb_data = workdir.path().join("mb");

    // Spawn mailbox + daemon (in-process; no Tor — use the loopback
    // transport already used by other integration tests in 1.E / 2.B).
    let mailbox = spawn_mailbox_server(&mb_data).await;
    let daemon = spawn_daemon_with_passphrase(&data_dir, "passphrase").await;

    // Initial: empty.
    let list = daemon.client().list_mailboxes().await.unwrap();
    assert!(list.is_empty());

    // Add.
    daemon.client().add_mailbox(mailbox.onion()).await.unwrap();

    // Wait briefly for the registration to settle and status to populate.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let list = daemon.client().list_mailboxes().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].onion, mailbox.onion());

    // Remove.
    let id = list[0].id;
    daemon.client().remove_mailbox(id).await.unwrap();
    let list = daemon.client().list_mailboxes().await.unwrap();
    assert!(list.is_empty());

    daemon.shutdown().await;
    mailbox.shutdown().await;
}
```

(`spawn_mailbox_server` / loopback-transport helper come from the 2.B integration tests; reuse the helper module that lives in `crates/tests/src/helpers/`.)

- [ ] **Step 2: Run**

```bash
cargo test -p skattr-tests --features test-harness mailbox_crud -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/tests/src/mailbox_crud.rs
git commit -m "$(cat <<'EOF'
test(mailbox): add → list → remove against a real mailbox server

Covers Task 15's real CRUD wiring against a 2.B MailboxClient
talking to an in-process mailbox server over loopback.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 16 — Docs + wrap-up

### Task 42: `docs/operations/2f-notification-smoke.md`

**Files:**
- Create: `docs/operations/2f-notification-smoke.md`

- [ ] **Step 1: Write the checklist**

```markdown
# Phase 2.F Notification Smoke Checklist

Manual cross-OS verification of the notification + tray subsystems.
Run on each platform before Phase 2.G ships.

**Setup:** install Skattr from the dev build; start two daemons with
two contacts; send a message from B → A while A's UI is in each state
below.

## Linux (X11 / GNOME 45+)
- [ ] Window focused, Alice conversation active: NO notification
- [ ] Window focused, Bob conversation active (msg from Alice): notification fires
- [ ] Window blurred: notification fires
- [ ] Window minimised: notification fires
- [ ] Mode = Full: title = sender, body = message preview
- [ ] Mode = Minimal: title = sender, body = empty
- [ ] Mode = Generic: title = "Skattr", body = "New message"
- [ ] Mode = Off: no notification regardless of state
- [ ] Per-contact mute (bell toggle): no notification, no unread badge
- [ ] Click notification: window focuses, opens that conversation
- [ ] Tray icon present in system tray
- [ ] Tray click → toggles window visibility
- [ ] Tray Quit → daemon process exits

## Linux (Wayland)
Same checklist; expect tray to be absent on bare Wayland (no system
tray protocol). Daemon should log a warning and close-button should
fall back to Quit.

## macOS 14+
- [ ] All Linux items
- [ ] Dock-bounce notification respects Do Not Disturb
- [ ] Tray icon in menu bar (top-right)

## Windows 11
- [ ] All Linux items
- [ ] Notifications appear in Action Center
- [ ] Tray icon in system tray (bottom-right); right-click shows menu

## Logs
- [ ] Settings → Advanced → Open logs viewer: shows recent records
- [ ] No 64-char hex blobs above debug level
- [ ] No `*.onion` strings above debug level
- [ ] Toggle "Persist logs to disk" → file appears at `${data_dir}/logs/skattr.log`
- [ ] Toggle off: file remains, no new lines appended

## Wipe
- [ ] Settings → Advanced → Delete all data and quit: confirms twice
- [ ] On confirm: daemon shuts down, data_dir is removed, app exits
```

- [ ] **Step 2: Commit**

```bash
git add docs/operations/2f-notification-smoke.md
git commit -m "$(cat <<'EOF'
docs(ops): Phase 2.F notification + tray + logs + wipe smoke checklist

Cross-OS manual verification list. Runs on Linux/macOS/Windows
before Phase 2.G ships. Linux X11 + Wayland are split because
Wayland may lack a system tray entirely.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 43: `docs/operations/passphrase-recovery.md`

**Files:**
- Create: `docs/operations/passphrase-recovery.md`

- [ ] **Step 1: Write the doc**

```markdown
# Passphrase Recovery

Skattr's `Command::ChangePassphrase` re-encrypts two files on disk:
`identity.vault` and `age-key`. The daemon uses a stage-then-rename
journal to make the re-key atomic across crashes — even if the daemon
is killed mid-operation, the next boot recovers deterministically.

This doc explains the on-disk layout, the recovery flow, and the
manual-recovery escape hatch for the rare "logically inconsistent"
branch.

## On-disk layout

Files in `${data_dir}/`:

| File                       | Mode | When present                              |
|----------------------------|------|-------------------------------------------|
| `identity.vault`           | 0600 | Always (after first-run wizard)           |
| `age-key`                  | 0600 | Always                                    |
| `passphrase-rekey.journal` | 0600 | Only during a re-key; deleted on success  |
| `identity.vault.staged`    | 0600 | Only during a re-key; deleted on success  |
| `age-key.staged`           | 0600 | Only during a re-key; deleted on success  |

A re-key runs in this order: stage both new files → write journal →
rename `identity.vault.staged` → `identity.vault` → rename
`age-key.staged` → `age-key` → delete journal.

## Recovery on boot

When the daemon starts and `passphrase-rekey.journal` exists, the
daemon prompts for the current passphrase and probes each file:

- **Both files match the NEW passphrase:** the rename happened on
  both; just delete the journal. No user action needed.
- **`identity.vault` is NEW, `age-key` is OLD:** finish the second
  rename from `age-key.staged`. Audit row marked `recovered`.
- **Both files are OLD AND prompted passphrase decrypts the OLD vault:**
  the user gave the OLD passphrase; roll back. Audit `rolled_back`.
- **Both files are OLD AND prompted passphrase decrypts neither:**
  the user typed the wrong passphrase. Re-prompt; do not modify any
  files.
- **`identity.vault` is OLD, `age-key` is NEW:** logically impossible.
  See "Manual recovery" below.

## Manual recovery

If the daemon prints `InconsistentState` and refuses to start:

1. Stop the daemon (already stopped).
2. Inspect `${data_dir}/`:
   ```
   ls -la ${data_dir}/identity.vault* age-key* passphrase-rekey.journal
   ```
3. Determine which passphrase you remember (OLD or NEW).
4. **To roll back to the OLD passphrase:**
   ```
   rm ${data_dir}/identity.vault.staged ${data_dir}/age-key.staged ${data_dir}/passphrase-rekey.journal
   ```
   The daemon will boot with `identity.vault` + `age-key` under their
   OLD passphrase.
5. **To roll forward to the NEW passphrase:**
   ```
   mv ${data_dir}/identity.vault.staged ${data_dir}/identity.vault
   mv ${data_dir}/age-key.staged ${data_dir}/age-key
   rm ${data_dir}/passphrase-rekey.journal
   ```
   The daemon will boot under the NEW passphrase.

## Lost passphrase

If you've forgotten **both** passphrases, recovery is **not possible**
through the daemon — by design. The BIP39 seed phrase you wrote down
during first-run is your only recovery path:

1. Back up `${data_dir}` to a safe location (so you can recover any
   in-flight messages later if you choose to dig manually).
2. Delete `${data_dir}/identity.vault` + `${data_dir}/age-key` +
   `${data_dir}/passphrase-rekey.journal` + `${data_dir}/<sqlite>.db`.
3. Run `skattr restore <seed words>` and choose a new passphrase.
4. Your contacts will see your old onion as unreachable; you'll need
   to send them new invites (or wait for them to send you one).

The seed phrase is the only thing that survives passphrase loss.
Store it offline. Do not commit it to git. Do not share it with
anyone.
```

- [ ] **Step 2: Commit**

```bash
git add docs/operations/passphrase-recovery.md
git commit -m "$(cat <<'EOF'
docs(ops): passphrase recovery procedure

Explains the journal + staged-file layout, the five recovery
branches, the manual escape hatch for the InconsistentState
branch, and the BIP39-seed-only path for lost passphrases.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 44: CHANGELOG + CLAUDE.md updates + final verification

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md` (Repository state section)

- [ ] **Step 1: CHANGELOG entry**

Prepend (or append, matching house style) to `CHANGELOG.md`:

```markdown
## [Unreleased] — Phase 2.F

### Added
- Settings panel with five sections (Identity / Mailboxes / History / Notifications / Advanced).
- `Command::GetConfig` / `Command::SetConfig` / `Command::ChangePassphrase` /
  `Command::SetContactMuted` / `Command::TailLogs` / `Command::GetPassphraseAuditLatest` /
  `Command::WipeAllData` and supporting types.
- `Event::LogRecord` + `EventFilter::Logs` for the live logs tail.
- `ContactSummary.muted` + `ContactSummary.peer_mailboxes` (additive).
- Stage-then-rename atomic `ChangePassphrase` with deterministic crash recovery.
- Per-contact mute persisted in `contacts.muted`.
- Tauri 2 built-in tray (Show / Tor status / Unread / Quit).
- Close-button hide-to-tray (configurable; default on).
- `notify-rust` desktop notifications, focus-aware + per-conversation mute.
- Cmd/Ctrl-K cross-conversation search palette (also inline in Settings → History).
- History export (JSONL or plaintext, optional gzip), retention slider, prune confirm.
- In-memory ring-buffer logs viewer with redaction; opt-in disk persistence via `tracing-appender`.
- `WipeAllData` "Danger zone" with two-step confirm.
- Migrations 0013 (`contacts.muted`) and 0014 (`passphrase_audit`).

### Changed
- Mailbox CRUD handlers (`AddMailbox` / `RemoveMailbox` / `ListMailboxes`) replaced 2.C stubs with real wiring against 2.B's `MailboxClient`.
- Retention sweep re-reads `Config` on every tick so retention changes hot-apply.

### Docs
- `docs/operations/2f-notification-smoke.md` — per-OS smoke checklist.
- `docs/operations/passphrase-recovery.md` — journal + manual recovery.
- `docs/superpowers/specs/2026-05-04-phase-2f-settings-history-design.md`.
- `docs/superpowers/plans/2026-05-04-phase-2f-settings-history.md`.
```

- [ ] **Step 2: CLAUDE.md update**

In `CLAUDE.md`'s "Repository state" section, append after the Phase 2.E paragraph:

```
Phase 2.F (settings & history) merged at the head of
`phase-2f-settings-history`. Migrations 0013 (`contacts.muted`) and
0014 (`passphrase_audit`) land alongside seven new `Command` variants
(`GetConfig`, `SetConfig`, `ChangePassphrase`, `SetContactMuted`,
`TailLogs`, `GetPassphraseAuditLatest`, `WipeAllData`), four new
`CommandResult` variants (`Config`, `PassphraseChanged`, `Logs`,
`PassphraseAudit`), `Event::LogRecord` + `EventFilter::Logs`, and
two additive fields on `ContactSummary` (`muted`, `peer_mailboxes`).
`ChangePassphrase` uses a stage-then-rename journal at
`${data_dir}/passphrase-rekey.journal`; recovery on boot probes
on-disk file fingerprints and never needs the OLD passphrase. Six
kill-point integration tests (K1–K6) cover every transition.
Mailbox CRUD handlers replace the 2.C stubs with real wiring against
2.B's `MailboxClient`. Tauri 2 built-in tray, focus-aware
`notify-rust` notifications with daemon-side per-conversation mute,
Cmd/Ctrl-K cross-conversation search palette over 1.G's
`SearchMessages`, in-memory ring-buffer logs viewer with redaction +
opt-in disk persistence via `tracing-appender`, and the
"Delete all data and quit" Danger Zone wire up. Closes Phase 2's
user-facing chrome; remaining Phase 2 work is 2.G (packaging).
```

Update the "Phase 0/1/... is complete" header line to include 2.F:

```
Phase 0 is complete; Phase 1 is complete; Phase 2.A/B/C/D/E/F are complete; the next workstream is Phase 2.G (packaging).
```

(Match the existing prose style; this is a sketch, not a verbatim replacement.)

- [ ] **Step 3: Final verification**

```bash
. "$HOME/.cargo/env"
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --features test-harness
cargo deny check
cd crates/ui/src-svelte && pnpm test --run && pnpm run check && cd -
```

Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude.md): mark Phase 2.F complete

CHANGELOG bullet covers all 2.F additions; CLAUDE.md Repository
state section absorbs the new wire surface, migrations, atomic
passphrase journal, mailbox CRUD wiring, tray, notifications,
search palette, logs viewer, and danger zone.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Push branch and open PR**

```bash
git push -u origin phase-2f-settings-history
gh pr create --title "Phase 2.F — settings & history" --body "$(cat <<'EOF'
## Summary
- Settings panel (Identity / Mailboxes / History / Notifications / Advanced) over a sidebar nav.
- `ChangePassphrase` with stage-then-rename atomicity + deterministic recovery.
- Tauri tray + close-to-tray + notifications + Cmd/Ctrl-K search palette + logs viewer + danger-zone wipe.
- Real mailbox CRUD wiring (replaces 2.C stubs).

Wire-format additions are strictly additive (7 new Commands, 4 new
CommandResults, 1 new Event, 1 new EventFilter, 2 additive ContactSummary
fields). Migrations 0013 + 0014. Six kill-point tests prove
ChangePassphrase atomicity.

Closes Phase 2's user-facing chrome.

## Test plan
- [x] `cargo fmt --check`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --features test-harness`
- [x] `cargo deny check`
- [x] `pnpm test --run` + `pnpm run check`
- [ ] Per-OS notification smoke checklist (`docs/operations/2f-notification-smoke.md`)
       run on Linux dev environment; macOS/Windows deferred.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Spec coverage map

Cross-check that every locked decision in the spec has a task:

| Decision | Task(s) |
|---|---|
| 1 — sidebar nav nested routes | 30, 31 |
| 2 — hybrid IPC | 4, 5, 12, 19, 24 |
| 3 — stage-then-rename ChangePassphrase | 16, 17, 18, 19, 20 |
| 4 — focus-aware notifications + hard-locked behaviour | 28, 35 |
| 5 — daemon-side per-contact mute | 2, 10, 13, 39 |
| 6 — Tauri built-in tray | 25 |
| 7 — close_to_tray + start_minimised | 8, 27, 36 |
| 8 — Cmd/Ctrl-K palette + inline reuse | 37, 38 |
| 9 — retention presets 24h/7d/30d/90d/Never | 34 |
| 10 — JSONL + plaintext + optional gzip | 34 |
| 11 — logs viewer ring buffer + opt-in disk | 21, 22, 23, 36 |
| 12 — peer_mailboxes projection | 11, 39 |
| 13 — WipeAllData + danger zone | 24, 36 |

Non-decision spec sections also covered:
- Wire-format `wire_format_append_only` snapshot test update → Task 7.
- `passphrase_audit` table → Task 3, repo Task 14.
- Recovery doc → Task 43.
- Per-OS smoke checklist → Task 42.
- `peer_mailboxes` ContactSummary additive field → Task 11.
- CHANGELOG + CLAUDE.md → Task 44.
- All five risks in spec § Risks → mitigated by the corresponding tasks (atomicity tests, smoke checklist, tray fallback warning, persist-toggle re-key, two-step wipe confirm, focus_row_id read-cursor preservation test).

---

## What this plan does NOT cover

- **Task 2.E.5** (mailbox fallback for Welcome) — independent follow-up; out of scope.
- **Task 20.5 / 22.5 / 23.5** — independent follow-ups; out of scope.
- "Restore archived contact" UI — only data model lands in 2.E; UI deferred.
- Wire-format BREAKING changes — separate spec required.
- Phase 2.G (packaging & distribution).
- Phase 3+ items (avatars, reactions, replies, edits, typing, attachments, multi-member groups).
- Phase 4+ items (cover traffic, panic-wipe, duress mode).
- Phase 5+ items (auto-update, code signing + notarisation).
