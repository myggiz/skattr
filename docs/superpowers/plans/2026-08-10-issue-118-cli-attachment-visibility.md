# #118 CLI Attachment Visibility + Save — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a received attachment visible and retrievable from the `skattr` CLI — render `Kind::File` rows with filename, size, id and availability instead of a raw `Debug` byte dump, and add `save-attachment` to write one to disk.

**Architecture:** Rendering stays a pure function of its inputs: availability is looked up from a map the caller supplies, never fetched inside the renderer. The caller (tail / export) probes availability over IPC and builds that map. `save-attachment` resolves a unique id prefix from recent messages, then acts on a fresh connection per the one-shot IPC contract.

**Tech Stack:** Rust 2021, clap, tokio, `skattr_core::AttachmentManifest`.

**Spec:** `docs/superpowers/specs/2026-08-10-issue-118-cli-attachment-visibility-design.md`

**Branch:** `118-cli-save-attachment` (created; spec committed as `3c16fb5`)

## Global Constraints

- **Purely CLI-side.** No new daemon command, no new IPC surface, no wire-format change, no migration. All four attachment commands already exist and are dispatched.
- **Rendering is pure.** A render function takes data in and returns a `String`. It must not perform IPC or filesystem access — the availability map is a parameter. (This is the standing functional-core rule, and the subject of #154.)
- **The IPC connection is single-request** (#116/#117). Every `execute` needs its own connection; resolve-then-act means two. In `--follow`, a probe *must* use a separate connection — Execute-after-Subscribe hangs.
- **Availability is probed for inbound rows only.** `attachment_available_cmd` requires `direction='in'`, so probing an outgoing row would print `incomplete` beside a file the user sent themselves.
- **A decode failure is never fatal** — render `📎 (unreadable manifest)` and continue. Same for a probe error: render the row without an availability field.
- **The destination path is absolutized CLI-side** (the daemon's cwd is not the CLI's). It is **not** validated — #54 owns that.
- **Respect the existing global `--json` flag** (`crates/cli/src/main.rs:40`).
- No `unwrap()`/`expect()` in non-test code. `cargo clippy -D warnings` is the done-gate.
- **The repo enforces DCO — every commit needs `git commit -s`.**
- Cargo is NOT on PATH: prefix every cargo command with `. "$HOME/.cargo/env" && `.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/cli/src/main.rs` | The whole CLI. Gains two pure render helpers, an availability parameter on the three renderers, a probe helper, and the `save-attachment` subcommand + handler. | Modify |
| `crates/tests/src/cli_save_attachment.rs` | **New.** Integration round-trip: a received attachment saves byte-identically. | Create |
| `crates/tests/src/lib.rs` | Test module registration. | Modify |

**Note on `main.rs`:** it is ~1900 lines and issue #70 already tracks splitting it into modules. This plan **does not** split it — that is a separate, tracked change and mixing it in would make this diff unreviewable. New helpers go beside their existing neighbours.

---

## Task 1: Pure render helpers + availability threading

**Files:**
- Modify: `crates/cli/src/main.rs` — add two helpers near `render_message_record_human` (~line 1088); change the signatures of `render_message_record_human`, `render_messages_human`, `render_export_text_line`; update their existing tests (~lines 1854-1995) and the three call sites (1053/1123/1177 via the renderers, and 1523 for export).

**Interfaces:**
- Consumes: `skattr_core::AttachmentManifest` (public re-export; fields `attachment_id: [u8; 16]`, `filename: String`, `total_size: u64` are all `pub`; `AttachmentManifest::from_cbor(bytes: &[u8]) -> Result<Self>`).
- Produces, used by Tasks 2 and 3:
  - `fn format_size(bytes: u64) -> String`
  - `fn render_file_kind(manifest: &[u8], availability: Option<bool>) -> String`
  - `type AvailMap = std::collections::HashMap<[u8; 16], bool>`
  - `fn render_message_record_human(row: &MessageRecord, avail: &AvailMap) -> String`
  - `fn render_messages_human(rows: &[MessageRecord], avail: &AvailMap) -> String`
  - `fn render_export_text_line(rec: &MessageRecord, avail: &AvailMap) -> String`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block in `crates/cli/src/main.rs`:

```rust
    fn test_manifest_bytes(name: &str, size: u64, id: [u8; 16]) -> Vec<u8> {
        use skattr_core::AttachmentManifest;
        let m = AttachmentManifest {
            manifest_version: 1,
            attachment_id: id,
            filename: name.to_string(),
            mime: "application/octet-stream".into(),
            total_size: size,
            chunk_size: 49152,
            file_key: [0u8; 32],
            chunks: vec![],
        };
        let mut buf = Vec::new();
        ciborium::into_writer(&m, &mut buf).unwrap();
        buf
    }

    #[test]
    fn format_size_renders_human_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KiB");
        assert_eq!(format_size(2_516_582), "2.4 MiB");
    }

    #[test]
    fn render_file_kind_shows_name_size_and_id() {
        let bytes = test_manifest_bytes("logs.tar.gz", 2_516_582, [0x78; 16]);
        let out = render_file_kind(&bytes, Some(true));
        assert!(out.contains("logs.tar.gz"), "got {out}");
        assert!(out.contains("2.4 MiB"), "got {out}");
        assert!(out.contains("id=78787878"), "got {out}");
        assert!(out.contains("available"), "got {out}");
    }

    #[test]
    fn render_file_kind_marks_incomplete_when_unavailable() {
        let bytes = test_manifest_bytes("huge.bin", 40, [0x4f; 16]);
        let out = render_file_kind(&bytes, Some(false));
        assert!(out.contains("incomplete"), "got {out}");
        assert!(!out.contains("available"), "must not claim available: {out}");
    }

    #[test]
    fn render_file_kind_omits_state_when_availability_unknown() {
        // Outgoing rows and failed probes pass None — the row still renders,
        // just without an availability field.
        let bytes = test_manifest_bytes("sent.bin", 10, [0x11; 16]);
        let out = render_file_kind(&bytes, None);
        assert!(out.contains("sent.bin"), "got {out}");
        assert!(!out.contains("available"), "got {out}");
        assert!(!out.contains("incomplete"), "got {out}");
    }

    #[test]
    fn render_file_kind_survives_an_undecodable_manifest() {
        // A corrupt or future-version manifest must not abort a tail.
        let out = render_file_kind(&[0xff, 0x00, 0x13], None);
        assert!(out.contains("unreadable manifest"), "got {out}");
    }

    #[test]
    fn render_messages_human_decodes_a_file_row() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let bytes = test_manifest_bytes("photo.jpg", 318_000, [0xAB; 16]);
        let rows = vec![MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::File { manifest: bytes },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        let mut avail = AvailMap::new();
        avail.insert([0xAB; 16], true);
        let out = render_messages_human(&rows, &avail);
        assert!(out.contains("photo.jpg"), "got {out}");
        assert!(out.contains("available"), "got {out}");
        // The old Debug dump must be gone.
        assert!(!out.contains("File {"), "still Debug-dumping: {out}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli`
Expected: FAIL — `cannot find function format_size` / `render_file_kind` / `AvailMap` (compile errors).

- [ ] **Step 3: Add the two pure helpers**

Insert immediately above `render_message_record_human` in `crates/cli/src/main.rs`:

```rust
/// Availability of inbound attachments, keyed by `attachment_id`.
///
/// Built by the caller (which does the IPC) and passed into rendering, so the
/// render functions stay pure and unit-testable.
type AvailMap = std::collections::HashMap<[u8; 16], bool>;

/// Human-readable byte size: `0 B`, `512 B`, `2.0 KiB`, `2.4 MiB`.
fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Render a `Kind::File` body: filename, size, short id, and (when known)
/// availability.
///
/// `Kind::File` carries only the CBOR manifest — there is no filename field on
/// the record — so decoding is the only way to say anything useful about it.
/// A manifest that will not decode renders as a marker rather than aborting the
/// listing: one bad row must not blind the whole tail (#118).
fn render_file_kind(manifest: &[u8], availability: Option<bool>) -> String {
    let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
        return "📎 (unreadable manifest)".to_string();
    };
    let id: String = m
        .attachment_id
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let state = match availability {
        Some(true) => "  available",
        Some(false) => "  incomplete",
        None => "",
    };
    format!(
        "📎 {name}  {size}  id={id}{state}",
        name = m.filename,
        size = format_size(m.total_size),
    )
}
```

- [ ] **Step 4: Thread the availability map through the three renderers**

In `render_message_record_human` (~line 1088), change the signature and the `body` match:

```rust
fn render_message_record_human(
    row: &skattr_core::daemon::commands::MessageRecord,
    avail: &AvailMap,
) -> String {
```

and replace the body match arm:

```rust
    let body = match &row.kind {
        Kind::Text { body } => body.clone(),
        Kind::File { manifest } => {
            render_file_kind(manifest, availability_for(manifest, avail))
        }
        other => format!("({other:?})"),
    };
```

Add this small lookup beside the helpers — it keeps the id-decode in one place:

```rust
/// Look up availability for a file row, keyed by the manifest's attachment id.
/// `None` when the manifest will not decode or the id was never probed
/// (outgoing rows, or a probe that failed).
fn availability_for(manifest: &[u8], avail: &AvailMap) -> Option<bool> {
    let m = skattr_core::AttachmentManifest::from_cbor(manifest).ok()?;
    avail.get(&m.attachment_id).copied()
}
```

Apply the same signature change and the same `Kind::File` arm to `render_export_text_line` (~line 1420), keeping its existing `<{other:?}>` fallback for other kinds.

Change `render_messages_human` (~line 1113) to take and forward the map:

```rust
fn render_messages_human(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    avail: &AvailMap,
) -> String {
```
and at its call to the per-row renderer: `render_message_record_human(row, avail)`.

- [ ] **Step 5: Update the existing call sites and tests**

Four call sites now need a map argument. For this task they all pass an empty one — probing arrives in Task 2, so behaviour changes only in that file rows now decode:

- `main.rs:1053` (tail non-follow) → `render_messages_human(&rows, &AvailMap::new())`
- `main.rs:1154` → `render_messages_human(&rows, &AvailMap::new())`
- `main.rs:1177` (tail --follow) → `render_message_record_human(&record, &AvailMap::new())`
- `main.rs:1523` (export) → `render_export_text_line(r, &AvailMap::new())`

Then update the pre-existing renderer tests (~lines 1854-1995) to pass `&AvailMap::new()`. Do not change what they assert — they are guarding existing text-row behaviour.

- [ ] **Step 6: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli`
Expected: PASS — the 6 new tests plus every pre-existing CLI test.

- [ ] **Step 7: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-cli --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/cli/src/main.rs
git commit -s -m "feat(cli): decode and render attachment rows instead of a Debug dump

Kind::File carries only the CBOR manifest and no filename, so tail and
export fell through to format!(\"({other:?})\") and printed the Debug of a
byte array — hundreds of integers, no name, no size, no id. The information
needed to act on a received attachment was present and unreadable, which is
what blinded the #115 test-bus.

Renders filename, human size and a short id. Availability is threaded in as
a map parameter rather than fetched inside, so the renderers stay pure and
directly unit-testable; the probing lands in the next commit.

An undecodable manifest renders as a marker rather than aborting the
listing — one bad row must not blind a whole tail.

Refs #118, #76"
```

---

## Task 2: Probe availability in tail, follow and export

**Files:**
- Modify: `crates/cli/src/main.rs` — add a probe helper; use it at the four call sites updated in Task 1.

**Interfaces:**
- Consumes from Task 1: `AvailMap`, `render_messages_human(rows, avail)`, `render_message_record_human(row, avail)`, `render_export_text_line(rec, avail)`.
- Consumes (existing): `connect_or_exit(sock_flag) -> IpcClient`, `CoreCommand::AttachmentAvailable { attachment_id: Hex16 }` → `CommandResult::AttachmentAvailability { available: bool }`.
- Produces: `async fn probe_availability(rows: &[MessageRecord], sock_flag: Option<&std::path::Path>) -> AvailMap`.

- [ ] **Step 1: Add the probe helper**

Add beside the other async helpers in `crates/cli/src/main.rs`:

```rust
/// Probe availability for every **inbound** file row in `rows`.
///
/// Inbound only: `attachment_available_cmd` answers true iff the row is
/// `direction='in', status='complete'`, so probing an outgoing row would print
/// `incomplete` beside a file the user sent themselves.
///
/// One probe per connection — the daemon's IPC connection is single-request
/// (#116), and in `--follow` the caller's connection is already subscribed, so
/// a probe there *must* be separate or it would hang.
///
/// Best-effort: a probe that fails is simply omitted from the map, and the row
/// renders without an availability field. Visibility must not be all-or-nothing.
async fn probe_availability(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    sock_flag: Option<&std::path::Path>,
) -> AvailMap {
    use skattr_core::daemon::commands::Direction;
    use skattr_core::envelope::Kind;

    let mut out = AvailMap::new();
    for row in rows {
        if row.direction != Direction::Incoming {
            continue;
        }
        let Kind::File { manifest } = &row.kind else {
            continue;
        };
        let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
            continue;
        };
        if out.contains_key(&m.attachment_id) {
            continue; // same attachment referenced twice in one listing
        }
        // Deliberately NOT `connect_or_exit`: it prints and exits the process
        // when the daemon is down, which is wrong for a best-effort probe.
        let Ok(path) = resolve_socket_path(sock_flag) else {
            continue;
        };
        let Ok(mut client) = skattr_core::daemon::IpcClient::connect(&path).await else {
            continue;
        };
        let res = client
            .execute(CoreCommand::AttachmentAvailable {
                attachment_id: skattr_core::daemon::hex::Hex16::from(m.attachment_id),
            })
            .await;
        if let Ok(CommandResult::AttachmentAvailability { available }) = res {
            out.insert(m.attachment_id, available);
        }
    }
    out
}
```

**Verified:** `connect_or_exit` (`main.rs:209`) resolves the path via `resolve_socket_path(sock_flag)?` and then calls `IpcClient::connect(&path)`, but prints and exits the process on `DaemonNotRunning`. That is right for a command and wrong for a best-effort probe, which is why the code above calls the two primitives directly and treats either failing as "skip this row".

- [ ] **Step 2: Use it at the four call sites**

- tail non-follow (~line 1053):
  ```rust
  let avail = probe_availability(&rows, sock_flag).await;
  print!("{}", render_messages_human(&rows, &avail));
  ```
- the second listing site (~line 1154): same two lines.
- tail `--follow` (~line 1177), per record:
  ```rust
  let avail = probe_availability(std::slice::from_ref(&record), sock_flag).await;
  println!("{}", render_message_record_human(&record, &avail));
  ```
- export (~line 1523): build the map once before the write loop from the rows being exported, then pass `&avail` to each `render_export_text_line` call.

- [ ] **Step 3: Verify no regression**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli`
Expected: PASS — all Task 1 tests still pass. The probe itself has no unit test (it is pure IPC plumbing); it is covered by the integration test in Task 4.

- [ ] **Step 4: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-cli --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -s -m "feat(cli): show whether a received attachment is available

tail, tail --follow and export now probe AttachmentAvailable for inbound
file rows and render 'available' or 'incomplete'.

Inbound only, because attachment_available_cmd answers true iff the row is
direction='in', status='complete' — probing an outgoing row would print
'incomplete' next to a file the user sent themselves.

One probe per connection: the IPC connection is single-request (#116), and
in --follow the caller's connection is already subscribed, so sharing it
would hang. A failed probe omits the row from the map and it renders
without an availability field rather than failing the listing.

Refs #118"
```

---

## Task 3: `save-attachment` subcommand

**Files:**
- Modify: `crates/cli/src/main.rs` — a `SaveAttachment` clap variant beside the other subcommands (~line 139), a dispatch arm, an id resolver, and the handler.

**Interfaces:**
- Consumes from Task 1: `format_size`.
- Consumes (existing): `CoreCommand::RecentMessages`, `CoreCommand::SaveAttachment { attachment_id: Hex16, dest_path: String }` → `CommandResult::Ok`, and the `connect_or_exit(sock_flag)` reconnect pattern.
- Produces: `fn resolve_attachment_id(rows: &[MessageRecord], prefix: &str) -> Result<([u8; 16], AttachmentManifest)>`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block (reusing `test_manifest_bytes` from Task 1):

```rust
    fn file_row(id: [u8; 16], name: &str) -> skattr_core::daemon::commands::MessageRecord {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::File {
                manifest: test_manifest_bytes(name, 10, id),
            },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }
    }

    #[test]
    fn resolve_attachment_id_matches_a_unique_prefix() {
        let rows = vec![file_row([0xAB; 16], "a.bin"), file_row([0xCD; 16], "b.bin")];
        let (id, m) = resolve_attachment_id(&rows, "abab").unwrap();
        assert_eq!(id, [0xAB; 16]);
        assert_eq!(m.filename, "a.bin");
    }

    #[test]
    fn resolve_attachment_id_is_case_insensitive() {
        let rows = vec![file_row([0xAB; 16], "a.bin")];
        assert!(resolve_attachment_id(&rows, "ABAB").is_ok());
    }

    #[test]
    fn resolve_attachment_id_ambiguous_reports_the_count() {
        // Two ids sharing the queried prefix.
        let mut a = [0u8; 16];
        a[0] = 0xAB;
        let mut b = [0u8; 16];
        b[0] = 0xAB;
        b[15] = 0x01;
        let rows = vec![file_row(a, "a.bin"), file_row(b, "b.bin")];
        let err = resolve_attachment_id(&rows, "ab").unwrap_err().to_string();
        assert!(err.contains('2'), "should report the count: {err}");
        assert!(err.to_lowercase().contains("ambiguous"), "got {err}");
    }

    #[test]
    fn resolve_attachment_id_no_match_errors() {
        let rows = vec![file_row([0xAB; 16], "a.bin")];
        let err = resolve_attachment_id(&rows, "ffff").unwrap_err().to_string();
        assert!(err.contains("ffff"), "should quote the prefix: {err}");
    }

    #[test]
    fn resolve_attachment_id_ignores_text_rows() {
        use skattr_core::daemon::commands::{Direction, MessageRecord};
        use skattr_core::daemon::hex::Hex16;
        use skattr_core::envelope::Kind;
        use skattr_core::identity::PublicKey;
        let rows = vec![MessageRecord {
            row_id: 0,
            message_id: Hex16::from([2; 16]),
            contact: PublicKey([7; 32]),
            direction: Direction::Incoming,
            kind: Kind::Text { body: "abab".into() },
            mls_generation: 0,
            ts_daemon_recv: 1_700_000_000,
            ts_envelope: 1_699_999_999,
        }];
        assert!(resolve_attachment_id(&rows, "abab").is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli resolve_attachment_id`
Expected: FAIL — `cannot find function resolve_attachment_id`.

- [ ] **Step 3: Implement the resolver**

Add beside `resolve_contact` (~line 994), mirroring its lowercase + `starts_with` + match-count-error shape:

```rust
/// Resolve a unique attachment-id prefix against the file rows in `rows`.
///
/// Mirrors `resolve_contact`: lowercased `starts_with` matching, and an error
/// naming the count when a prefix is ambiguous. Returns the full id and the
/// decoded manifest, so the caller can report the filename and size without
/// decoding twice.
fn resolve_attachment_id(
    rows: &[skattr_core::daemon::commands::MessageRecord],
    prefix: &str,
) -> Result<([u8; 16], skattr_core::AttachmentManifest)> {
    use skattr_core::envelope::Kind;
    let lower = prefix.to_ascii_lowercase();
    let mut matches: Vec<([u8; 16], skattr_core::AttachmentManifest)> = Vec::new();
    for row in rows {
        let Kind::File { manifest } = &row.kind else {
            continue;
        };
        let Ok(m) = skattr_core::AttachmentManifest::from_cbor(manifest) else {
            continue;
        };
        let hex: String = m.attachment_id.iter().map(|b| format!("{b:02x}")).collect();
        if hex.starts_with(&lower) && !matches.iter().any(|(id, _)| *id == m.attachment_id) {
            matches.push((m.attachment_id, m));
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => anyhow::bail!("no attachment matches {prefix:?}"),
        n => anyhow::bail!("ambiguous: {n} attachments match {prefix:?}"),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli resolve_attachment_id`
Expected: PASS — 5 tests.

- [ ] **Step 5: Add the subcommand and handler**

Add a clap variant beside the other subcommands (the `Send`/`Tail`/`Export` group, ~line 110-145):

```rust
    /// Save a received attachment to a file.
    SaveAttachment {
        /// Attachment id, or any unique prefix of it (see `tail`).
        id: String,
        /// Destination path. Relative paths resolve against the current
        /// directory.
        dest: String,
    },
```

Add the dispatch arm alongside the others in the `match cli.command` block, matching the surrounding style:

```rust
        Command::SaveAttachment { id, dest } => {
            save_attachment(&id, &dest, cli.socket.as_deref(), cli.json).await
        }
```

Add the handler beside the other async handlers:

```rust
/// Save a received attachment to `dest`.
///
/// Resolve-then-act: one connection to list recent messages (to resolve the id
/// prefix), a second to save. The IPC connection is single-request (#116), so
/// reusing the first would broken-pipe.
async fn save_attachment(
    id_prefix: &str,
    dest: &str,
    sock_flag: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    // 1. Resolve the prefix against recent messages.
    let mut client = connect_or_exit(sock_flag).await?;
    let rows = match client
        .execute(CoreCommand::RecentMessages {
            contact: None,
            limit: 500,
            before_id: None,
            paged: false,
        })
        .await
    {
        Ok(CommandResult::Messages(rows)) => rows,
        Ok(other) => anyhow::bail!("unexpected reply: {other:?}"),
        Err(e) => exit_on_ipc_error(e),
    };
    let (attachment_id, manifest) = resolve_attachment_id(&rows, id_prefix)?;

    // 2. Absolutize the destination. The daemon's working directory is not
    //    ours, so a relative path would otherwise resolve somewhere the user
    //    did not mean. Validation is #54's job, not this command's.
    let dest_path = std::path::Path::new(dest);
    let abs = if dest_path.is_absolute() {
        dest_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(dest_path)
    };

    // 3. Save on a fresh connection.
    let mut client = connect_or_exit(sock_flag).await?;
    match client
        .execute(CoreCommand::SaveAttachment {
            attachment_id: skattr_core::daemon::hex::Hex16::from(attachment_id),
            dest_path: abs.to_string_lossy().into_owned(),
        })
        .await
    {
        Ok(CommandResult::Ok) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "saved": true,
                        "path": abs.to_string_lossy(),
                        "filename": manifest.filename,
                        "size": manifest.total_size,
                    })
                );
            } else {
                println!(
                    "saved {} -> {}",
                    format_size(manifest.total_size),
                    abs.display()
                );
            }
            Ok(())
        }
        Ok(other) => anyhow::bail!("unexpected reply: {other:?}"),
        // Verified: `decrypt_attachment_to` (dispatch.rs) returns
        // `IpcError::Daemon(DaemonErrorKind::InvalidArgument{..})` when the row
        // is missing OR when `direction != "in" || status != "complete"`.
        // Since the id came from a real file row we just listed, the live cause
        // is effectively always "not complete yet". Report it as a clean
        // diagnostic with a non-zero exit so a script can branch on it, rather
        // than an error dump (#118 acceptance).
        Err(skattr_core::daemon::IpcClientError::Server(
            skattr_core::daemon::ipc::wire::IpcError::Daemon(
                skattr_core::daemon::error_kind::DaemonErrorKind::InvalidArgument { .. },
            ),
        )) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "saved": false, "reason": "unavailable" })
                );
            } else {
                eprintln!("not available yet (transfer incomplete)");
            }
            std::process::exit(1);
        }
        // Anything else is a genuine transport/daemon failure.
        Err(e) => exit_on_ipc_error(e),
    }
}
```

**Verified against the source, so use as written:** `CoreCommand::RecentMessages` takes **four** fields — `contact`, `limit`, `before_id`, `paged` (see the existing `tail` call at `main.rs:1033`). The error helper is `exit_on_ipc_error(err) -> !` at `main.rs:226`, which diverges — hence it is the fallback arm, after the `InvalidArgument` arm has claimed the unavailable case.

**Check only this one shape against the source:** the exact `IpcClientError::Server(...)` wrapping in the `Err` arm. `IpcClientError::Server(IpcError)` is the variant used in `crates/tests/src/cli_ipc_roundtrip.rs`; confirm the nesting compiles and adjust the pattern if the enum differs. If matching proves awkward, an acceptable fallback is to match on the rendered error string containing the daemon's invalid-argument message — but prefer the typed match.

- [ ] **Step 6: Verify the whole CLI suite**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-cli`
Expected: PASS.

- [ ] **Step 7: Gate**

```bash
. "$HOME/.cargo/env" && cargo fmt --all -- --check \
  && cargo clippy -p skattr-cli --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/cli/src/main.rs
git commit -s -m "feat(cli): add save-attachment

Writes a received attachment to a chosen path. The id argument accepts any
unique prefix, mirroring resolve_contact's lowercase starts_with matching
and its ambiguous/no-match error shape.

Resolve-then-act on two connections, per the single-request IPC contract
(#116) — reusing the first is the bug that broke eight commands.

The destination is absolutized against the CLI's cwd before being sent: the
daemon's working directory is not ours, so a relative path would otherwise
be written somewhere the user did not mean. Validation is left to #54
rather than half-done here.

A not-yet-complete attachment prints a clean diagnostic and exits non-zero
so a script can branch on it, rather than dumping an error.

Refs #118"
```

---

## Task 4: Integration round-trip + docs + gate

**Files:**
- Create: `crates/tests/src/cli_save_attachment.rs`
- Modify: `crates/tests/src/lib.rs` (module registration)
- Modify: `CLAUDE.md` (the #118 limitation line, if present)

**Interfaces:** consumes the CLI surface from Tasks 1-3.

- [ ] **Step 1: Read the harness you will reuse**

The acceptance is a byte-identical round-trip: receive an attachment, save it, compare bytes. `crates/tests/src/attachment_transfer_direct.rs` already stands up a real transfer — reuse it rather than building a new harness. Its imports are:

```rust
use skattr_core::daemon::{Command, CommandResult, IpcClient, Ready};
use skattr_core::test_exports::{run_loopback, LoopbackNet};
use crate::loopback_harness::{config_for, init_vault, wait_for_group_active, PASSPHRASE};
```

Read that file first and follow its setup verbatim; only the assertions at the end differ.

- [ ] **Step 2: Write the failing test**

Create `crates/tests/src/cli_save_attachment.rs` with the GPLv3 SPDX header, and a test that:
1. Runs a transfer to completion using the existing harness, capturing the original file bytes and the `attachment_id`.
2. Calls `Command::SaveAttachment { attachment_id, dest_path }` with a `tempfile::tempdir()` destination, via the same handle the harness exposes.
3. Asserts the saved file's bytes are **exactly equal** to the original — and additionally that their sha256 matches, since the manifest's integrity guarantee is what the acceptance names.
4. A second case: calling `SaveAttachment` for an attachment that is still `'pending'` returns an error rather than writing a partial file, and asserts the destination does **not** exist afterwards.

Register the module in `crates/tests/src/lib.rs` beside the existing entries.

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `. "$HOME/.cargo/env" && cargo test -p skattr-tests cli_save_attachment`
The first run should fail with the module or test missing; after implementing, it must pass. Report both outputs.

- [ ] **Step 4: Update the docs if #118 is named as a limitation**

Search `CLAUDE.md` for the #118 note (the "CLI has NO save/open-attachment verb, so received attachments are invisible via CLI" phrasing appears in the attachment limitations area). If present, correct it to say the CLI can now list and save received attachments, and that `open`/`retry` verbs remain unimplemented. If it is not present, skip this step and say so — do not invent a bullet.

- [ ] **Step 5: Full local gate**

```bash
. "$HOME/.cargo/env" \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings \
  && cargo test \
  && cargo deny check
```
Expected: every command exits 0. Capture the counts for the PR body.

- [ ] **Step 6: Commit**

```bash
git add crates/tests/src/cli_save_attachment.rs crates/tests/src/lib.rs CLAUDE.md
git commit -s -m "test(cli): byte-identical save-attachment round-trip

The #118 acceptance is that a saved attachment matches the original
byte-for-byte. Drives a real transfer through the existing harness, saves
it, and compares both the raw bytes and their sha256 against the manifest.

Also covers the negative case: saving a still-pending attachment errors and
leaves no partial file at the destination.

Refs #118"
```

**Do NOT push or open the PR** — the maintainer handles that.

---

## Self-Review

**1. Spec coverage**

| Spec section | Task |
|---|---|
| §3.1 render filename/size/short id | Task 1 (`render_file_kind`, `format_size`) |
| §3.2 decode failure is not fatal | Task 1 (`render_file_kind_survives_an_undecodable_manifest`) |
| §3.3 availability, inbound only, one probe per connection, probe error tolerated | Task 2 (`probe_availability`) |
| §4.1 unique-prefix id resolution mirroring `resolve_contact` | Task 3 (5 resolver tests) |
| §4.2 destination absolutized CLI-side, not validated | Task 3 Step 5 |
| §4.3 output, non-zero exit on unavailable, `--json` | Task 3 Step 5 |
| §5 unit tests (resolver, renderer, undecodable) | Tasks 1 and 3 |
| §5 integration byte-identical round-trip | Task 4 |
| §6 acceptance mapping | Tasks 1-4 |
| §7 exclusions (no list cmd, no retry, no CLI-side sha256 on save, no caching) | No task adds any |

No gaps.

**2. Placeholder scan:** No TBD/TODO. Every code step carries literal code. The first draft carried three "check this against the source" hedges; all were resolved by reading the source rather than left to the implementer:

- `connect_or_exit` (`main.rs:209`) exits on `DaemonNotRunning`, so the probe calls `resolve_socket_path` + `IpcClient::connect` directly.
- `CoreCommand::RecentMessages` takes **four** fields, not two — my first sketch would not have compiled.
- The error helper is `exit_on_ipc_error(err) -> !` (diverging), so it is the fallback arm rather than a message formatter.
- The `attachment_transfer_direct.rs` harness imports are quoted verbatim in Task 4.

One hedge remains, deliberately: the exact `IpcClientError::Server(IpcError::Daemon(...))` nesting in Task 3's unavailable arm. The variant names are taken from `cli_ipc_roundtrip.rs`, but I did not compile the nested pattern, so the step says to confirm it and offers a stated fallback.

**3. Type consistency:** `AvailMap = HashMap<[u8; 16], bool>` is defined in Task 1 and used with that exact type in Tasks 1-2. `render_file_kind(&[u8], Option<bool>) -> String`, `format_size(u64) -> String`, and `resolve_attachment_id(&[MessageRecord], &str) -> Result<([u8; 16], AttachmentManifest)>` are each defined once and consumed with matching signatures. `Hex16::from([u8; 16])` matches the construction used at `daemon/inbound.rs:891`. The `MessageRecord` literal in the test fixtures matches the field set of the existing test at `main.rs:1866` exactly, including `row_id`, `mls_generation`, `ts_daemon_recv`, `ts_envelope`.

**The risk flagged in the first draft is now resolved by reading the code.** `decrypt_attachment_to` (`daemon/dispatch.rs`) returns `Err(IpcError::Daemon(DaemonErrorKind::InvalidArgument{..}))` when `row.direction != "in" || row.status != "complete"`, so `SaveAttachment` does error for a not-yet-complete attachment — verified, not inferred.

One nuance carried into the code comment: that same `InvalidArgument` is also returned when the row is missing entirely, so the two causes are indistinguishable from the error alone. Since the id was resolved from a file row we had just listed, "not complete yet" is effectively always the live cause — but the message says "transfer incomplete" rather than claiming certainty about a row that exists. Task 4's negative-case test pins the behaviour either way.
