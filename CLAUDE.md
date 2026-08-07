# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

There are **two eras** in this project's history. Read them as: *(1) a
feature-complete-looking original build that the v1.0 readiness audit found
had a "dead production path", then (2) the audit-driven re-phasing that is the
current, authoritative story.*

### Era 1 — the original build (history; complete)

The original implementation plan ran phases **0 → 2.H** and is **done**. In
order: 0.A scaffold, 0.B identity & crypto, 0.C Arti transport, 0.D storage,
0.E docs; 1.A frame codec, 1.B Noise_XK handshake, 1.C MLS groups (2-member),
1.D invite + ContactCard, 1.E delivery hub/outbox/receiver, 1.F daemon IPC +
CLI, 1.G message storage + FTS5 search + retention, 1.H hardening; then the
old "Phase 2" UI track — 2.A mailbox server, 2.B mailbox client + ContactCard
rotation, 2.C UI bootstrap, 2.D conversation view, 2.E invite/contact UX, 2.F
settings & history, 2.G packaging (Linux `.deb`/AppImage/Flatpak + macOS
`.dmg`), 2.H Windows port (Named Pipes IPC + `.msi`). This produced all four
`core`/`mailbox`/`cli`/`tests` crates plus the Tauri 2 + SvelteKit `ui` crate,
the frozen mailbox wire protocol (ADR 0006), the `age`-encrypted storage layer,
and the release CI matrix. These old-phase labels are **retired** — do not use
"Phase 2.x" to mean the UI track anymore; it now means the audit's security
workstream (below).

### The v1.0 readiness audit (2026-06-12)

An eight-domain audit (`docs/V1.0-READINESS-AUDIT.md`, scope locked in
`docs/superpowers/specs/2026-06-12-v1.0-roadmap.md`) found **"green tests, dead
production path"**: the messenger UI and two-daemon flows passed only because
tests hand-wired the transport via `skattr_core::test_exports`; **nothing wired
the inbound/outbound transport into `Daemon::run`**. The audit re-phased all
remaining work toward a shippable v1.0 (a 1:1 / 2-member, attachment-capable,
Tor-only messenger) with a strict dependency chain **Phase 1 → 2 → 3 → 4** and
one cross-cutting rule: **every phase must prove its behavior through the real
`Daemon::run` (`run_with_transport`) assembly over loopback — not via
`test_exports`.** That live guardrail is introduced in Phase 1 and extended in
every later phase.

### Era 2 — the audit phases (current)

#### Phase 1 — Make messaging work (T0 functional) — ✅ complete

Two real daemons exchange messages in both directions through production wiring.

- **1A — inbound correctness** (merge `4936b1e`): the T0 inbound fixes — the
  onion accept loop resolves `peer_x25519 → Ed25519 → ContactCard` and rejects
  any unknown/unauthenticated peer before `DeliveryHub::ingest`; decrypt →
  persist → emit is made correct. Extracted `poll_dispatch_once`
  (fetch → dispatch → delete-only-dispatched) so a transient failure can't drop
  a deposit.
- **1B — direct P2P transport** (merge `b12f7ea`): the production seam.
  `daemon::state::run_with_transport<T: Transport>` owns `Pool` + `DeliveryHub`
  + accept loop + IPC; `transport::{Transport, arti_transport::ArtiTransport,
  loopback::LoopbackTransport}` and `delivery::dial::OutboundDial`
  (`TransportDial`) wire dialing into `Daemon::run`. Direct-only guardrail:
  `two_daemons_exchange_messages_both_directions_over_loopback` drives the real
  assembly.
- **1C — first contact** (merge `5c0b827`): invite → add → Welcome →
  bidirectional between *previously unknown* peers. The invite embeds the
  inviter's signed `ContactCard` so the consumer learns the onion (ADR 0008);
  the accept loop has a first-contact `Welcome` carve-out (ADR 0007) that
  authenticates + binds the derived identity before join. Guardrail:
  `first_contact_invite_add_then_bidirectional_over_loopback`. Followed by a
  shared-harness cleanup (`c70a511`, `crates/tests/src/loopback_harness.rs`).

#### Phase 2 — Critical security & data-integrity (T1 + named T2) — ✅ complete

Decomposed in `docs/superpowers/specs/2026-06-13-phase-2-decomposition.md` into
four independent sub-projects, each spec → ADR (where protocol/auth changed) →
plan → subagent-driven execution → live guardrail → merge.

- **2.A — MLS ratchet & binding integrity** (merge `bc71f32`; spec
  `2026-06-13-phase-2a-mls-integrity-design.md`, ADR 0009):
  - **T1-1** `h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")`
    is now injected as an external MLS PSK on the genesis commit, **active and
    mandatory**, via a *dial-first two-PSK construction* (the invitee dials the
    inviter, captures `h_transport`, injects both the invite PSK and the
    `h_transport` PSK into the `add_member` genesis commit; the responder
    derives the identical transcript value and registers it before
    `join_from_welcome`).
  - **T1-3** per-group ratchet serialization — a `group_id`-keyed
    `std::sync::Mutex` registry (`GroupLockRegistry`) shared by one `Arc` across
    send (`DaemonHandle`) and receive (`DaemonInbound`); guard dropped before
    any `.await`.
  - **T2-2** inbound-Commit tolerance — `Group::decrypt` returns
    `Result<Option<Envelope>>` and merges a `StagedCommit` (advances epoch)
    instead of erroring; `can_receive` split from `can_send`.
  - **T2-8** per-invite PSK uniqueness — ids/nonces derived from the invite's
    `KeyPackageRef`.
  - **T2-1** single-use atomicity — `add_contact` is one `pool.transaction`
    with an in-txn `is_consumed` re-check; dial-by-onion-from-invite means a
    dial failure leaves zero writes for a clean retry.

- **2.C — offline delivery: fallback + drain** (merge `18b7f36`; spec
  `2026-06-14-phase-2c-offline-delivery-design.md`):
  - **T1-6** direct→mailbox fallback wired into production: the non-generic
    `delivery::hub::MailboxFallbackShared` (built in `run_with_transport`), a
    per-peer **sustained-failure timer** in `delivery::peer` that fires
    `run_mailbox_fallback` after `direct_timeout_secs` of unbroken failure
    (this closes the old **Task 20.5**), and a dedicated
    `delivery::mailbox_sweeper` that re-deposits due mailbox-kind outbox rows
    with per-mailbox failover + backoff. `OutboxEntry`/`OutboxRow` now carry
    `target_kind`/`mailbox_id`, and the direct retry tick skips mailbox-kind
    rows.
  - **T1-4** `RemoveMailbox` drains held deposits through
    `InboundDispatch::dispatch_mailbox` (via `poll_dispatch_once`,
    delete-only-dispatched) before finalizing removal — closes the old **Task
    22.5**.
  - **ts-replay poison fix** — the mailbox delivery path is exempt from the
    ±1h `Envelope.ts` window (legitimately-delayed deposits surface);
    replay resistance comes from `(sender, envelope_id)` dedup + MLS generation
    + server delete. The direct path keeps the ±1h window.
  - Guardrail: `offline_peer_receives_via_mailbox_fallback` (peer offline →
    mailbox → poll → receive, through the real assembly).

- **2.D — resource hardening (anti-flood)** (merge `2a38ed8`; spec
  `2026-06-15-phase-2d-resource-hardening-design.md`):
  - **T1-5** mailbox-server caps (AGPLv3 crate) — five operator-tunable
    `Policy` knobs: `global_storage_cap_bytes`, `max_recipients`,
    `idle_timeout_secs`, `max_connections`, `max_delete_ids`. Global byte cap +
    recipient-count cap enforced atomically in `Store::insert`
    (**reject-after-expired** — never evicts an accepted, non-expired message);
    idle-connection timeout in `accept_loop`; a load-shedding connection
    semaphore on `MailboxServer::serve_connection`; bounded `Delete.deposit_ids`.
  - **accept-loop spawn bound** — the daemon's inbound accept loop bounds
    concurrent handshakes with a `Semaphore` (backpressure) and drains in-flight
    tasks via a `JoinSet` on shutdown.
  - Wire-format neutral: new internal `PolicyErrorKind` variants map to
    existing `ErrorCode`s (ADR 0006 frozen). Guardrail: `mailbox_flood`.

- **2.B — at-rest encryption lifecycle** (merge `65baf87`; spec
  `2026-06-15-phase-2b-at-rest-encryption-design.md`):
  - **T1-2** the `age`-encrypt-on-shutdown path is now actually reached.
    `Pool.conn` → `Mutex<Option<Connection>>` so an explicit close and a `Drop`
    backstop can both `take()` it. `Pool::close(&self)` is idempotent +
    guarded: `wal_checkpoint(TRUNCATE)` → drop conn → encrypt → remove the
    plaintext DB + `-wal`/`-shm` sidecars + sentinel. `run_with_transport`
    teardown calls `pool.close()` **deterministically** through the retained
    `Arc` (no `try_unwrap` race); `Drop` is the backstop for abnormal exits.
    `Pool::open` writes a `skattr.sqlite.open` sentinel and re-encrypts crash
    residue on boot, so a current `.age` always exists. `export_backup` now
    works (it depended on a real `.age`). Guardrail:
    `clean_shutdown_leaves_only_encrypted_db` (no plaintext/sidecars/sentinel
    after a clean shutdown).

#### Phase 3 — Attachments — ✅ complete (3.A, 3.B, 3.C, 3.D done)

File attachments (send / receive / preview) with metadata stripping; a new
`envelope::kinds` attachment variant; chunking/transfer over the hardened
transport + mailbox path. Depends on Phase 2 being closed (it is). Decomposed
into 3.A → 3.B → 3.C → 3.D.

- **3.A — attachment core** (merge `9a132d1`; final whole-branch review "Ready
  to merge", gate green — core 636/0, skattr-tests 39/0, cargo-deny ok): the
  local, transport-free pipeline. The manifest rides in MLS via `Kind::File`;
  chunk blobs are opaque AEAD ciphertext keyed from the manifest. New
  `crates/core/src/attachment/` (`chunker`, `manifest`, `reassembler`, `store`
  (`ChunkStore` stage), `strip` (metadata stripping), `error_kind`),
  `storage::attachments` (`AttachmentRepo` per-chunk receipt state, migration
  `0015_attachments`). Local round-trip validated without transport.
- **3.B — direct attachment transfer** — ✅ done (branch
  `phase-3b-direct-attachment-transfer`; final whole-branch review "Ready to
  merge"; gate green — core lib 655/0, skattr-tests pass incl. the loopback
  guardrail; spec `2026-06-17-phase-3b-direct-attachment-transfer-design.md`,
  ADR 0010). The online, both-peers-reachable path, **pull/request-driven**:
  four additive transport `FrameType`s `0x0B`–`0x0E` (the free bytes start at
  `0x0B` — `0x0A` is `Error`) — `ChunkRequest`/`Chunk`/`ChunkNack`/
  `AttachmentComplete` carrying opaque, Noise-encrypted (**not** MLS-wrapped)
  chunk ciphertext, sha256-verified against the manifest before storage.
  `Command::SendFile { contact, path }` (strip → chunk → stage in `ChunkStore`
  → persist `AttachmentRepo` `out` row → announce the manifest as a `Kind::File`
  MLS message). New `delivery::chunk_transfer` (`ChunkRx` window/retry state
  machine, `serve_chunk_request`, `sanitize_filename`, `unique_download_path`)
  is driven inside the per-peer `delivery::peer` actor (serve / windowed-fetch
  N=8 / one-attachment-per-peer FIFO / in-session resume via `ReplaceConn` →
  `reissue` / 30 s request timeout). Receiver auto-fetches → **stages the
  encrypted chunks in the `ChunkStore` (encrypted-at-rest; NOT reassembled to
  `download_dir`)** → emits
  `Event::{AttachmentReceived,AttachmentProgress,AttachmentFailed}` — where
  `AttachmentReceived` signals *availability*. Plaintext is produced only on an
  explicit `SaveAttachment`/`OpenAttachment` (`attachment_available_cmd` reports
  a received attachment available iff its row is `direction="in", status="complete"`,
  i.e. iff `finalize_rx` ran). An empty `download_dir` after a receive is expected,
  not a failure. Verified two-machine over real Tor in v0.1.10 (see #76).
  **`CHUNK_SIZE` is 48 KiB** (reduced from 3.A's 256 KiB so one chunk fits one
  Noise message — `connection::send` caps inner frames at 65 519 B; a regression
  guard `chunk_frame_worst_case_fits_noise_max_outer` locks it). No new
  migration (3.A's `0015` covers receiver receipt state; sender uses `ChunkStore`
  + the `out` row). Guardrails: `attachment_roundtrip_multichunk_over_loopback`
  (real `run_with_transport`, byte-identical multi-chunk + EXIF stripped) and an
  actor-level in-session-resume test (a full-stack reconnect could not be
  deterministically triggered). Deferred hardening: scope `serve_chunk_request`
  to `direction='out'` (done as a final guard), offline-manifest/online-chunks
  gap is 3.C.
- **3.C — offline transfer** — ✅ done (branch
  `phase-3c-offline-attachment-transfer`; final whole-branch review "Ready to
  merge"; gate green — core lib + skattr-tests pass incl. two offline
  guardrails; spec `2026-06-22-phase-3c-offline-attachment-transfer-design.md`;
  **no ADR** — reuses frozen ADR 0006 `Deposit` unchanged). Chunks reach an
  offline peer via the mailbox: each chunk is deposited as the raw 3.A AEAD blob
  (opaque, addressed to `recipient_hash = sha256(pubkey)`, `ttl=0`) — **no wire
  metadata**. The receiver identifies a fetched deposit by `sha256`-matching it
  against pending `direction='in'` manifests (the match *is* the integrity
  check), in `DaemonInbound::dispatch_attachment_chunk` (tried before
  `dispatch_mailbox` in `poll_dispatch_once`); a non-match is left on the server.
  Sender durable state: new `attachment_deposits` table (migration `0016`, +
  an `attachments.peer` column) + `AttachmentDepositRepo`; `SendFile` eagerly
  enqueues **deferred** deposit rows (`next_retry_at = now + 90 s` stall window;
  a direct 3.B `AttachmentComplete` prunes them first). `delivery::chunk_sweep`
  (sibling to `mailbox_sweeper`, spawned in `run_with_transport`) deposits due
  chunks from `ChunkStore` with per-mailbox failover + bounded backoff, pruning
  on all-deposited. Offline files are capped at **`MAX_OFFLINE_ATTACHMENT_BYTES`
  = 10 MiB** (larger → direct-only, wait for both online); deposit-all + the
  receiver's `received_indices` dedup makes the offline and 3.B direct lanes
  compose idempotently. Cross-session resume: sender via `attachment_deposits`
  (`chunk_sweep` re-queries `due()` on boot), receiver via `attachment_chunks`.
  A stalled inbound stays `pending` (no janitor — deferred with 3.B's
  partial-GC). Guardrails (component-level vs a real `InProcessMailbox`, matching
  2.C's offline pattern; full `run_with_transport` offline is impractical):
  `offline_attachment_via_mailbox` (byte-identical multi-chunk + EXIF stripped
  through real `run_chunk_sweep`/`poll_dispatch_once`/`dispatch_attachment_chunk`/
  `reassemble`) and `offline_attachment_cross_session_resume` (receiver restart
  mid-transfer resumes from durable state).
- **3.D — UI** — ✅ done (branch `phase-3d-attachment-ui`; final whole-branch
  review "Ready to merge"; gate green — `skattr-ui` clippy clean + 14/14 Rust
  tests, vitest 111/111, e2e 13/13; spec
  `2026-06-23-phase-3d-attachment-ui-design.md`, plan
  `2026-06-23-phase-3d-attachment-ui.md`; **no ADR, no core/protocol change** —
  presentation + IPC wiring only). Surfaces the 3.A/B/C core in the Tauri 2 +
  SvelteKit UI. Four UI-shell `#[tauri::command]`s in `crates/ui/src/attachments.rs`
  (`decode_attachment_manifest` → canonical `AttachmentManifest::from_cbor` via a
  minimal `pub use` re-export; `file_size`; `open_file`/`reveal_in_folder` via the
  `opener` plugin with canonicalize+regular-file validation). New SvelteKit units:
  `stores/attachments.ts` (global session-scoped transfer store keyed by hex
  `attachment_id`, mirrors `delivery.ts`), `lib/attachments.ts` (formatBytes /
  mime helpers / `decodeManifest` wrapper + per-message memo),
  `components/FileAttachmentBubble.svelte` (sender card + delivery status /
  receiver progress / inline image preview via the Tauri asset protocol /
  Open-Reveal / failed / decode-unavailable). Wiring: `Composer.svelte` paperclip
  → `@tauri-apps/plugin-dialog` picker → pre-send size gate (>100 MiB block,
  10–100 MiB soft-warn; daemon's `MAX_ATTACHMENT_BYTES` stays authoritative) →
  optimistic `Kind::File` bubble + `SendFile`; `MessageBubble.svelte` switches
  `kind==="file"` → the file bubble; `+page.svelte` gains 3 dispatcher arms for
  `Event::Attachment{Progress,Received,Failed}`. **The `Kind::File.manifest` is a
  runtime byte array** (serde_json number array; the ts-rs `string` type is a
  `#[ts(type="string")]` annotation) — the UI passes it straight to the Rust
  decoder, never base64. Tauri config: `dialog`+`opener` plugins, `protocol-asset`
  feature, an asset-protocol scope confined at runtime to `<data_dir>/downloads`,
  `img-src` CSP gains `asset: http://asset.localhost`, minimal
  `capabilities/default.json`. CI: `pnpm test` (vitest) added as a hard gate to
  the `ui` job. Deferred to v1.1 (see limitations): confine
  `open_file`/`reveal_in_folder` to the downloads dir (currently safe-by-context —
  paths are daemon-authored from `AttachmentReceived` + `script-src 'self'`); a
  unit test for the indeterminate "Downloading…" (≤8-chunk) bubble branch; plus
  the design's own deferrals — post-restart received-attachment state
  (session-scoped store), configurable download folder, in-UI retry, sender-side
  download progress, concurrent attachments per peer.

#### Phase 4 — Release integrity, docs, signing — ✅ complete (4.D, 4.B, 4.C, 4.A done; real signing keys landed in #20)

Honest, accurate user-facing docs (close the audit's documentation-truthfulness
gaps), a working download-verification chain, and **real signing keys**. The
signing story is now shipped: PR #20 ("release(signing): real minisign key; drop
PGP for v0.1.0") generated the **real** minisign keypair (`docs/install/minisign.pub`,
key ID `EEDBFDA4BF232D38`, secret held offline per
`docs/install/README-MAINTAINER-MINISIGN.md`) replacing the earlier placeholder,
and **dropped PGP for v0.1.0** (plaintext-email disclosure per `SECURITY.md`).
Signed releases **v0.1.0** and **v0.1.1** were cut, so the `SHA256SUMS` +
`SHA256SUMS.minisig` verification chain is live.

The loose-end disposition for v1.0 is locked in
`docs/superpowers/specs/2026-06-23-v1.0-pull-forward-vs-disclose-decisions.md`
(governing bar: cheap security/correctness fixes pulled into v1.0, real
protocol/architecture work disclosed as v1.1). The pulled-forward fixes
(P1/P2/P3/P5) populate **4.D**; disclosures (D1/D2/D3) populate **4.B**; the D1
client-side mitigation goes in **4.C**.

- **4.D — T3 security hardening** — ✅ done (merge PR #7 `fe4c83b`; spec
  `2026-06-23-phase-4d-t3-hardening-design.md`, plan
  `2026-06-23-phase-4d-t3-hardening.md`; **no ADR / no wire-format change** —
  `SchemaTooNew` reuses the existing `DaemonErrorKind::StorageError`). Nine
  hardening items, subagent-driven (TDD per task + independent spec/quality
  review + opus whole-branch review) then babysat through CI + two CodeRabbit
  rounds: **Item 4** schema-downgrade guard (`migrations::apply` refuses a DB
  whose `schema_version` exceeds the highest known migration, and propagates a
  read error rather than masking it as fresh); **P2** attachment-completion CAS
  fire-gate (`AttachmentRepo::set_status_if_pending`; both finalize lanes now
  run the CAS *first* and only the winner allocates a `unique_download_path` +
  reassembles, so a losing/erroring lane writes no orphan and the direct/offline
  lanes can't race on the same output path — this also closes the old "3.C
  completion not atomic across lanes" duplicate-event gap at the file level);
  **Item 1** IPC `map_err` logs only the error category (redaction-bypass fix);
  **Item 2** `sign_cbor`/`verify_cbor` zeroize their CBOR scratch buffers;
  **Item 5** `current_uid` uses infallible `libc::getuid()` (drops the
  `/proc`→`$UID`→`0`-root fallback on the IPC peer-cred boundary); **P1**
  `open_file`/`reveal_in_folder` confined to `<data_dir>/downloads` (dual-
  canonicalize + component-wise `starts_with`, Tauri-injected `State`); **Item
  3** `data_dir` created mode `0700` on unix; **P3** `warn!` on the six
  swallowed `chunk_sweep` writes; **P5** `PromotedMessage` type removes an
  unsound `as unknown` cast. The dropped HS-key audit finding has no task (the
  spec rejected it with evidence). Remaining non-blocking follow-ups tracked for
  v1.1/4.B: add `warn!` to the CAS no-emit-on-`Err` paths; tighten the
  `add_contact`/`send_file` log-safety doc-comments re `IpcError::Internal`;
  let `conversation.ts` `send()` adopt `PromotedMessage`.
- **4.B — documentation truthfulness** — ✅ done (merge PR #9 `d2850ef`; spec
  `2026-06-25-phase-4b-doc-truthfulness-design.md`, plan
  `2026-06-25-phase-4b-doc-truthfulness.md`; **pure docs — no code change**).
  Corrected every user-facing + audit-flagged internal doc against the shipped
  v1.0 code, subagent-driven (TDD-adapted: prove each claim false against the
  code source-of-truth → correct → verify) + an opus whole-branch truthfulness
  review. **README** dropped the false "groups scale to ~50 members" (2-member
  gate `mls/group.rs:115`) and the "seed-derived address" overclaim (the onion
  key is `OsRng`-random, persisted under a seed-derived *storage* key —
  `transport/hs_key.rs`), and now carries the v1.0 limitations. **THREAT_MODEL**
  re-stamped to v1.0: withhold-detection downgraded to latent (no alerting code
  exists), the stable recipient-hash mailbox-correlation leak disclosed
  (`mailbox/client.rs:272`), the Identity-stability guarantee corrected (seed
  restores the Ed25519 identity + history *while the DB file is intact*; the
  `.onion` needs the backup), D1/D2/D3 + the standing v1.1 list added.
  **SECURITY/install** disclose the minisign/PGP keys are placeholders (chain
  not usable yet). **ARCHITECTURE/PROTOCOL** facts fixed (migrations `0016`,
  ciphersuite codepoint `0x0003`, phase table). **deep-dives** superseded
  banners; **design** ciphersuite name corrected. **passphrase-recovery** (the
  safety-critical one) rewritten: the DB key is seed-derived and
  passphrase-independent (`storage/pool.rs:77`), so it now leads with
  seed-restore — intact files recover history with no backup; deletion is
  demoted to a last-resort data-loss warning. Tracked v1.1 follow-up (code, out
  of 4.B scope): the stale `hs_key.rs:13` "deliberate rotation" comment
  contradicts the degenerate shipped `RotateOnion`.
- **4.C — UI robustness & data-safety UX** — ✅ done (merge PR #11 `68a7039`;
  spec `2026-06-25-phase-4c-ui-robustness-design.md`, plan
  `2026-06-25-phase-4c-ui-robustness.md`; **no peer-facing protocol change** —
  the new `Command::ExportBackup` is additive append-only local IPC). Four items,
  subagent-driven + opus whole-branch review: **(A)** the Tauri bridge
  (`ipc_bridge.rs`) preserves the structured `IpcError` instead of flattening to
  `Internal`; a frontend `errorMessage(IpcError)` maps `DaemonErrorKind` to human
  strings (AddContactDialog shows specific invite/Tor messages). **(B)** the event
  relay (`events.rs`) emits `ipc:stream-closed` on death (was a silent `break`); a
  `connection` store re-subscribes with bounded backoff (concurrent-guarded) + a
  reconnect banner — a transient IPC hiccup self-heals. **(C)** the D1 mitigation
  (waiting-state only, no auto-retry): `add_contact` dial failure → `DeliveryTimeout`
  so the dialog shows "both must be online" with a clean re-submit; a "Connecting…"
  badge while `group_state==pending_join`. **(D)** GUI backup + gated wipe: a
  boot-derived `DaemonHandle.backup_key` (`Option<Zeroizing>`, fail-closed),
  `Pool::snapshot_encrypted` (`VACUUM INTO` → storage-key encrypt) +
  `export_backup_from_parts` (CLI-restorable archive), `Command::ExportBackup` run
  via `spawn_blocking` with a unique temp + guaranteed cleanup, a Settings
  "Export backup…" action, a three-way wipe gate (offer backup first; cancel/fail
  can't reach the wipe), and a deterministic wipe teardown after the flushed IPC
  `Bye` (replacing the 150 ms sleep, audit T3-3). Also fixed three pre-existing CI
  flakiness sources surfaced during the babysit (the 30s→60s daemon-readiness
  timeouts; the `from_url_rejects_tampered_signature` `'A'`-byte no-op tamper).
  Remaining v1.1 follow-ups: full clean-teardown reroute for wipe (background
  tasks); the `errors.ts` default-arm / `take_wipe_target` naming notes.
- **4.A — release/CI completeness** — ✅ done (merge PR #13 `19c5b52`; spec
  `2026-06-25-phase-4a-release-ci-completeness-design.md`, plan
  `2026-06-25-phase-4a-release-ci-completeness.md`; **CI/test/packaging only — no
  product/protocol change, no ADR**). Makes the project's own quality evidence
  actually gate merges, subagent-driven + opus whole-branch review. **Item A**
  the 13 Playwright e2e specs (mock-backend, `TAURI_MOCK=1`) now run as a hard
  gate in the `ui` job (`playwright install --with-deps chromium` → `pnpm
  test:e2e`; ran 13/13 green in CI for the first time). **Item B** `pnpm check`
  (svelte-check) is now a hard gate at **0 errors / 0 warnings** — root-fixed the
  4 `ConfigPatch.download_dir` type errors (the ts-rs field annotation made
  nullable-in-patch, `download_dir: null` set at 4 call sites) and cleared the 4
  a11y warnings (SearchPalette + mailboxes overlay); `package.json check` gains
  `--fail-on-warnings`. **Item D** the P4 coverage hole — a Vitest case for
  `FileAttachmentBubble`'s indeterminate "Downloading…" branch (`applyProgress(AID,
  0, 0)` → `.indeterminate` + label, no `.bar`). **Item C** a separate
  `.github/workflows/flatpak.yml` (master-push + weekly cron + `workflow_dispatch`,
  **not** per-PR) to keep the in-repo Flatpak manifest from silently rotting.
  Real minisign/PGP key generation stayed an explicit **non-goal** (the
  `release.yml` signing plumbing is wired; key material is a pre-tag maintainer
  action). **Flatpak follow-up** (fix-forward, merge PR #14 `00b4a4b`): the new
  `flatpak.yml`'s first authoritative master run failed — it caught real manifest
  rot (the pinned `org.freedesktop.*//23.08` runtime's `rust-stable` is frozen at
  Cargo 1.81, too old for the current dep tree which needs `edition2024`/Rust ≥
  1.85). Per the disclose-don't-overclaim philosophy and since the `.flatpak`
  isn't shipped in v1.0, the **full** build-validation was deferred to v1.1; the
  workflow now only **parse-checks** the manifest (`flatpak-builder
  --show-manifest`, no toolchain/runtime/network — fast + green), and
  `docs/build/flatpak.md` documents the exact v1.1 fix (runtime bump 23.08→24.08
  with Rust ≥ 1.85 + the build-phase `--share=network` grant). See the
  Deferred-status entry.

### Deferred / known-limitation status

- **Task 20.5** (per-peer direct-timeout → mailbox fallback) — ✅ done in 2.C.
- **Task 22.5** (`RemoveMailbox` final-drain dispatch) — ✅ done in 2.C.
- **Task 23.5** (real onion-key rotation) — ❌ still deferred (v1.1).
  `Command::RotateOnion` bumps the self-card version and republishes the
  *current* onion; contacts see a new `ContactCardReceived` version but route
  to the same address. See the `dispatch.rs` doc-comment.
- **First-contact `Welcome` is direct-only** — there is no mailbox fallback for
  the first-contact `Welcome` frame (old Task 2.E.5); if the inviter is offline
  when the joiner sends the Welcome, first contact stalls. Deferred (touching it
  would extend the ADR 0006 freeze). Ordinary messages and ContactCard updates
  do have mailbox fallback (2.C). **Partial recovery shipped in #93 (ADR 0012):**
  a **lost Ack** self-heals — if the inviter joined but its acknowledgement was
  lost, a re-sent Welcome is idempotently re-Acked. A **lost first Welcome** over
  a since-replaced circuit does **not** recover: the re-sent Welcome carries the
  original connection's `h_transport` (ADR 0009) and cannot bind on a new
  connection; the invitee stays "Connecting…" and keeps retrying (capped
  backoff). Tracked with #90 Mode A; full recovery is v1.1.
- **3.C completion is not atomic across lanes** — ❌ deferred (v1.1). The
  attachment-complete check (`received_indices().len() >= total` → reassemble +
  emit `AttachmentReceived` → `set_status('complete')`) is not atomic with the
  status flip, and the direct (3.B `finalize_rx`) and offline (3.C
  `finalize_offline`) lanes run in separate tasks. Under *simultaneous*
  direct+offline completion a duplicate `AttachmentReceived` event can fire —
  event-level only (no corruption; `unique_download_path` writes distinct files;
  UI keys on `attachment_id`). Fix: a compare-and-set status gate
  (`UPDATE … WHERE status='pending'`, rows-affected as the fire gate). Also v1.1:
  add `warn!` on the `chunk_sweep` prune writes + `finalize_offline`
  post-reassemble status writes (currently swallowed with `let _ =`).
- **3.C offline transfer is best-effort** — a deposited-but-never-fetched
  attachment is lost after the mailbox TTL (~7 days; the sender gets no fetch
  feedback so it never re-deposits), and a stalled inbound stays `pending`
  forever (no auto-fail/partial-GC janitor — shared deferral with 3.B). Large
  files (>10 MiB) cannot transfer while a peer is offline. All disclosed in the
  v1.0 limitations.
- **Flatpak full build-validation is deferred (manifest does not build today)** —
  ❌ deferred (v1.1). The in-repo manifest (`packaging/flatpak/net.myggiz.skattr.yml`)
  pins the `org.freedesktop.*//23.08` runtime, past freedesktop's support window
  and frozen at **Cargo 1.81** — too old for the current dep tree (`tauri-cli
  2.11.0` pulls `edition2024` crates needing **Rust ≥ 1.85**), so a sandbox build
  fails to compile. The `.flatpak` isn't shipped in v1.0 (Flathub publication is
  itself v1.1-deferred; the v1.0 install story is build-from-source / `release.yml`'s
  `.deb`/AppImage/`.dmg`/`.msi`). 4.A's `flatpak.yml` therefore only **parse-checks**
  the manifest (`flatpak-builder --show-manifest`) — it does *not* build it. v1.1
  fix (documented in `docs/build/flatpak.md`): bump the runtime 23.08→24.08 (Rust
  ≥ 1.85), grant build-phase network via the manifest's `build-options.build-args:
  [--share=network]` (there is **no** `flatpak-builder --share-network` CLI flag),
  and restore the full sandbox build in `flatpak.yml`.
- **v1.1+ deferrals** (must be disclosed as absent in the v1.0 threat model):
  third-party security audit; metadata-minimization (message-size padding,
  send-timing jitter, cover traffic / cover polling); multi-member groups (>2);
  real onion-key rotation; reactions / edit / delete-for-everyone / typing /
  read receipts (the `Kind` placeholders stay inert); multi-device.

### Module landmarks (audit-era additions)

`crates/core/src` now includes, beyond the original tree:
`daemon::state::run_with_transport` (the production assembly + deterministic
pool-close teardown), `daemon::accept` (bounded accept loop),
`daemon::{logs, retention, smoke, clock}`, the per-platform
`daemon::ipc::{client,server}::{unix,windows}` (Named Pipes + DACL + SID),
`delivery::{dial::OutboundDial, mailbox_sweeper, hub::MailboxFallbackShared,
peer, chunk_transfer, chunk_sweep}` (the sustained-failure timer;
`chunk_transfer` = 3.B's pull `ChunkRx` + serve; `chunk_sweep` = 3.C's offline
chunk-deposit sweeper), `mls::group` (two-PSK genesis +
`can_receive`), `mailbox::{client, codec, poll, auth}` (the v1-protocol client),
and `storage::{pool (Option<Connection> + WAL-safe close + Drop +
sentinel/re-encrypt-on-boot), passphrase_audit, outstanding_invites,
read_state}`. Migrations run through `0019` (`0015_attachments` in Phase 3.A;
`0016_attachment_deposits` + the `attachments.peer` column in Phase 3.C;
`0017_pending_welcomes`, `0018_first_contact_acks`, `0019_pending_welcome_failed`
from the #93/#107 first-contact recovery work). ADRs 0007 (first-contact Welcome
carve-out), 0008 (invite embeds ContactCard), and 0009 (`h_transport`↔MLS
binding) anchor the audit-era protocol decisions.

### Conventions, invariants, build state

`crates/core/src/identity/` is fully implemented (Ed25519, BIP39, Argon2id +
XChaCha20-Poly1305 vault, HKDF). The daemon is driven by `Daemon::run` →
`run_with_transport`; the CLI is a thin wrapper. `transport`, `storage`, `mls`,
`mailbox`, `delivery` are `pub(crate)`; integration tests reach internals via
`skattr_core::test_exports` gated on the `test-harness` feature, but **every
audit-phase behavior is also proven through a live `run_with_transport`
guardrail** (the audit's defining rule). `cargo clippy -D warnings` / `cargo
test` / `cargo fmt --check` are green across the workspace; CI (`ci.yml`) runs
on `ubuntu-latest` only, plus a `ui` job. Windows-only compile/lint errors are
caught by the local Windows build loop on myggdesk before tagging; macOS is not
built in CI (no Mac hardware). `release.yml` builds/signs on `ubuntu-latest` +
`windows-latest` at tag time (`.deb`/AppImage/`.msi`; no `.dmg`). The
bootstrap prompt remains authoritative for file layout, module boundaries, and
type signatures — match it exactly.

## Authoritative docs (read these first)

Work is driven by four docs in `docs/`; they have clear roles — don't invent structure these files don't cover:

- `skattr-bootstrap-prompt.md` — exact Cargo workspace layout, file-by-file module tree, key types with method signatures, dependency list, initial SQL migration, success criteria for the scaffold. This is the spec for "make the project compile."
- `skattr-design.md` — protocol spec: wire framing, Noise_XK handshake, MLS binding, invite link format, mailbox threat model. Source of truth for *what the protocol does*.
- `skattr-implementation-plan.md` — phased workstreams (0 through 5) with per-phase locked decisions, exit checklists, and risks. Source of truth for *what to build in what order*.
- `skattr-deep-dives.md` — detailed design for the `core` module layout, MLS group state machine, mailbox wire protocol, and first-run UX. Consult before touching those areas.

When these docs disagree, the design doc wins for protocol semantics; the bootstrap prompt wins for initial file layout and scaffolding.

## Development process

Use the `superpowers` skills by default for every development task — they are rigid workflows, don't skip them for "simple" work:

- **Before creating, designing, or changing behavior** → `superpowers:brainstorming` (explore intent before code).
- **Multi-step tasks with a spec** → `superpowers:writing-plans`, then `superpowers:executing-plans`.
- **Writing implementation code** → `superpowers:test-driven-development`.
- **Any bug, test failure, or unexpected behavior** → `superpowers:systematic-debugging`.
- **Before claiming work complete / committing / opening a PR** → `superpowers:verification-before-completion` (run the commands, show the output, no success claims without evidence).
- **Receiving code review feedback** → `superpowers:receiving-code-review` (verify before implementing).
- **2+ independent tasks** → `superpowers:dispatching-parallel-agents`.

The `using-superpowers` skill itself enforces "invoke relevant skills BEFORE any response or action" — treat that as binding, not advisory.

**When a phase or task is complete: push the branch and open a PR.** The PR is the unit of review and the record of what shipped; keep opening them even though CI is on-demand. **There is currently no automated PR reviewer** — CodeRabbit was removed (2026-08-07) and a replacement (Greptile or similar) is being evaluated. Until one is in place, the second pair of eyes is the `superpowers` whole-branch review on the most capable model, run before the PR is opened — do not skip it, and do not treat a green local gate as a substitute for it. When a new reviewer is adopted, restore the babysit rule here: verify findings before applying, reject false positives with evidence, and resolve all threads before merging.

### Issue tracking (GitHub issues are the backlog)

The work backlog lives in **GitHub issues** (`gh issue ...` on `myggiz/skattr`), not only in docs. Treat them as the source of truth for what to build next.

- **Milestones** group the roadmap: **`v0.1.2`** (near-term product + cheap security/correctness fixes), **`v1.1`** (larger protocol/feature/perf work), **`polish`** (non-functional cleanup: dead code, duplication, doc drift, test hardening, UX polish). Add a milestone only when a genuinely new track appears.
- **Topic labels**: `security`, `attachments`, `ci`, `tor`, `protocol`, `ux`, `data-path`, `performance`, `tech-debt`, `tests`, plus the defaults (`bug`, `enhancement`, `documentation`). Label by subsystem + kind.
- **File findings as issues, don't let them rot in a doc.** Any review, audit, or brainstorming outcome that names concrete fixable work becomes issue(s) with a body that carries: context/source, `file:line`, the problem, a suggested direction (described, not pre-coded), and acceptance criteria. Reviews live in `docs/` (e.g. `docs/f_review.md`) *and* seed issues.
- **Granularity:** one issue per substantive item; bundle a cluster of trivial one-liners that share a single fix-motion (all doc-drift, all dead-code) into **one checklist issue** (`- [ ]` per finding) so it's workable rather than 50 micro-issues. Cross-reference already-tracked issues instead of duplicating (e.g. "relates to #38").
- **Close the loop:** before starting a task, check for an existing issue; a PR that resolves issues references them (`Closes #NN`) so merge auto-closes them. When new work is agreed mid-session, file the issue first, then implement.
- Don't re-file the known-limitation deferrals already catalogued in this file and in `docs/` unless promoting one to active work.

### Global coding standards (also binding)

Personal/global standards live at `~/.claude/rules/standards/` (`rust.md`, `typescript.md`, `restraints.md`) and are meant to auto-attach by path glob — but that mechanism doesn't always fire, so the load-bearing rules are mirrored here (they bind regardless):

- **Rust** (`src/**/*.rs`): newtypes over primitives; model states as **enums, not bool flags**; **no `unwrap`/`expect` outside tests**; errors are **our types, never a vendor's**; **test-first** (the test must fail before the fix); **`clippy -D warnings` is the done-gate**. *Functional core / imperative shell:* **no I/O, clock, randomness, or env reads inside logic — take them as parameters and wire concretes up in `main`** (the review flagged several existing violations, e.g. `now_ms`/`paths`/`group.rs` RNG — fix these as we touch that code, don't retrofit wholesale).
- **TypeScript** (`**/*.{ts,tsx}`): `strict`; **no `any`, no `!`, no `ts-ignore`**; **branded types for IDs**, parse at the boundary; discriminated unions over optional-field soup; **`tsc --noEmit` + eslint are the done-gate**.
- **Restraint** (everything): bias to the **smallest change that works**; **don't refactor/rename/tidy code you weren't asked about — say what you'd change and let the maintainer decide**; no speculative abstraction / single-impl interfaces / factories where a plain function does; no defensive handling for cases that can't happen (fail loudly); leave no dead code or TODO stubs; **if a rule makes the code worse here, say so and write the simpler version**; if the request is ambiguous, ask.

### Build & release cadence (sub-versions) — local-first, on-demand CI

To conserve GitHub Actions minutes during the sub-version fix cycle, **CI is verified locally, not by Actions on every push.**

- **CI is on-demand.** `ci.yml` and `flatpak.yml` trigger on `workflow_dispatch` only (no auto PR/push runs). **The local gate is authoritative** — before calling any change done, run: `cargo fmt --all -- --check`, `cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings`, `cargo test`, `cargo clippy -p skattr-ui --all-targets -- -D warnings`, and (for UI) `pnpm check` + `pnpm exec vitest run`, plus `cargo deny check`. The maintainer builds/tests the **Windows** target locally on a separate box. **Keep opening PRs** — they remain the review unit and the shipping record, even with no automated reviewer attached (see above).
- **Run Actions deliberately, not by default.** Trigger a full CI run (Actions tab → CI → *Run workflow*) only when a change specifically warrants a cross-platform cross-check, or before a real release. Reserve the expensive tag-triggered `release.yml` (signed multi-platform build) for **major/blessed releases (1.0, 2.0, …)** or when explicitly requested — a local field-test build does **not** need a tag.
- **Versioning: SemVer, patch-bump per build.** `MAJOR.MINOR.PATCH`, each component an **integer** (so `0.1.9 → 0.1.10 → 0.1.47`; there is no "`.9` wall" — you go to `1.0`/`2.0` by decision, not by running out). Bump **PATCH** for each field-test build (bugfixes), **MINOR** when a batch of features lands, **MAJOR** for a blessed/breaking milestone. A bump is a local edit to `Cargo.toml` (`workspace.package.version`) + `crates/ui/tauri.conf.json` (`version`) + `Cargo.lock` (via `cargo check`) + a `CHANGELOG.md` entry listing the issues that build closes (`#NN`). The git commit/issue links are the record; the version number is the build tracker.

## Model routing

The Superpowers skills (notably `subagent-driven-development` and
`dispatching-parallel-agents`) describe model tiers in abstract terms — "most
capable model", "standard model", "cheap model". This section is the
**authoritative mapping** of those tiers to concrete models for this repo:

| Abstract tier (Superpowers) | Concrete model | Use for |
|---|---|---|
| **most capable model** | **opus** (`claude-opus-4-8`) | architecture, design, brainstorming, planning, spec/quality/whole-branch review, complex reasoning, anything touching crypto/protocol/auth |
| **standard model** | **sonnet-4-6** (`claude-sonnet-4-6`) | integration, multi-file coordination, debugging, most implementation tasks once the plan is well-specified |
| **cheap model** | **haiku-4-5** (`claude-haiku-4-5`) | mechanical implementation touching 1–2 files against a complete spec |

Routing rules:

- **Subagents spawned via `superpowers:dispatching-parallel-agents` default to
  sonnet-4-6**, unless the task is explicitly architectural (design / review /
  cross-cutting reasoning) — those go to opus.
- In `subagent-driven-development`, pick the lowest tier that fits the task's
  complexity signals (1–2 files + complete spec → haiku-4-5; multi-file /
  integration → sonnet-4-6; design/review → opus). Reviews (spec compliance,
  code quality, final whole-branch) are judgment work — prefer opus.
- When unsure, round **up** a tier: a wrong cheap-model result costs more than
  the model-tier savings.

## What Skattr is

A Rust, desktop-first, metadata-resistant P2P encrypted messenger. All traffic goes over Tor v3 onion services (via Arti). Message encryption is MLS (RFC 9420) via OpenMLS. Transport auth is Noise_XK via `snow`. Identity is an Ed25519 keypair backed by a BIP39 seed phrase. No central server; mailboxes exist only for offline delivery and are semi-trusted. Licensed GPLv3 (client) / AGPLv3 (mailbox server). Owned by Myggiz B.V. (Netherlands).

## Locked technical decisions (do not casually revisit)

These are decided and changing them has cascading consequences. Full rationale lives in the design doc and implementation plan's "Decisions to lock" tables.

- **Edition / toolchain:** Rust 2021, stable, pinned via `rust-toolchain.toml`.
- **Async runtime:** Tokio (Arti requires it).
- **Tor:** Arti (`arti-client` + `tor-hsservice`). Fallback to shelling out to system `tor` is documented in workstream 0.C but **not** something to architect around unless Arti blocks you.
- **Noise pattern:** `Noise_XK_25519_ChaChaPoly_BLAKE2s` (via `snow`) — plain XK, `psk = None` at every production call site. (A `Noise_XKpsk3_…` constant exists but no production path selects it; the invite PSK is applied at the MLS layer, not in Noise.) Identity keys are the Noise static keys — **distinct from onion service keys** (see design §1.1).
- **MLS ciphersuite:** `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`. Note: the design doc mentions a 256-bit variant in prose; the bootstrap prompt and Phase 1 decision lock the 128-bit variant — use the 128-bit one.
- **Crypto libraries:** `ed25519-dalek`, `chacha20poly1305` (XChaCha20-Poly1305), `argon2`, `hkdf`, `sha2`, `blake2`, plus `age` for at-rest encryption. Argon2id params: `m=64MiB, t=3, p=4` (written per-file into the vault CBOR, so a file's own params govern its decrypt). `x25519-dalek` is a declared dep, but production X25519 runs through `snow`'s resolver and a hand-derived static — our own use of the crate is a derivation cross-check test.
- **Seed phrase:** BIP39 — the 32-byte root secret is *entropy-encoded* as 24 words (no BIP39 passphrase, no PBKDF2 stretching stage). Derivation: `HKDF-SHA256(salt=∅, ikm=seed, info="skattr-identity-v1") → ed25519 seed → keypair`. Domain-separate every HKDF use (eight distinct `INFO_*` constants live in `identity/derive.rs`).
- **Wire serialization:** CBOR via `ciborium`. Config: TOML.
- **Storage:** `rusqlite` (bundled) with WAL mode + app-level encryption via `age`. Migrations are `include_str!`'d SQL keyed by a `schema_version` table.
- **Errors:** `thiserror` in libraries, `anyhow` in binaries. **No `unwrap()` / `expect()` in library code** — use `?` and typed errors. Enforced by `clippy::unwrap_used`/`expect_used` + `-D warnings`. Three deliberate exceptions exist, each with a local `#[allow]` and a SAFETY comment (compile-time-constant regexes in `daemon/logs.rs`, invite-URL serialization in `daemon/commands.rs`, a bounds-checked cast in `transport/frame.rs`); add one only with the same justification.
- **Logging:** `tracing` + `tracing-subscriber`. Never log pubkeys, onions, or message contents at `info` level or higher; redaction by default.
- **Invite URI scheme:** `skattr://invite/v1#...` (fragment-based to avoid referer leaks).
- **Transport↔MLS binding:** `h_transport = HKDF(noise_handshake_hash, "skattr-binding-v1")` is injected as external PSK into the first MLS Commit. Preserve this binding when refactoring either layer.

## Workspace layout (target)

A Cargo workspace with these crates (see bootstrap prompt for the full tree):

- `crates/core/` — the library where almost all logic lives (identity, transport, mls, envelope, invite, contact, mailbox client, delivery, storage, daemon). Licensed GPLv3.
- `crates/mailbox/` — standalone server binary. Shares `core::mailbox::protocol` types. Licensed **AGPLv3**.
- `crates/cli/` — thin `clap`-based binary wrapping `core::Daemon`. Licensed GPLv3.
- `crates/tests/` — integration tests (spawn daemon pairs, wait for Tor bootstrap).
- `crates/ui/` — **reserved, do not scaffold in Phase 0**. Tauri 2 + SvelteKit lands in Phase 2.

## Module visibility discipline

In `core`, only these are public API: `daemon`, `identity` (key types only), `envelope`, `invite`, `contact`, `error`. Everything else is `pub(crate)`. If something inside `transport/`, `mls/`, `mailbox/`, `delivery/`, or `storage/` needs to be exposed, wrap it in a public type from one of the approved modules rather than widening visibility.

## Non-obvious hard constraints

- **Every `.rs` file must carry a license header comment.** GPLv3 for `core`/`cli`/`tests`, AGPLv3 for `mailbox`.
- **All secret types zeroize.** Wrap keys, seeds, passphrases, derived key material in `Zeroizing` or implement `ZeroizeOnDrop`. No raw `[u8; 32]` secrets sitting on the stack un-zeroed.
- **No custom crypto.** No hand-rolled Noise patterns, no "small tweaks" to MLS, no hand-rolled AEAD. Where the design doc says "use X," use X.
- **MLS state is fragile.** Treat MLS storage like a database: transactions, WAL, explicit recovery paths. A single bad write can brick a group — see deep-dives Part 2 for the state machine (`Active`, `PendingJoin`, `PendingCommit`, `CatchingUp`, `Removed`, `Corrupt`).
- **Timestamps are display-only.** Authoritative ordering comes from MLS generation numbers, not `Envelope.ts`. Validate `ts` within ±1h for replay resistance; don't sort by it.
- **Invite KeyPackages are single-use.** Mark consumed on first successful use; reject on second.
- **The scaffold must pass `cargo clippy -D warnings`** and `cargo test` (even with `todo!()`-stubbed bodies) before being considered done.
- **Workspace-level `dead_code = "allow"` and `unused_imports = "allow"` are intentional during Phase 0** (see `Cargo.toml` comment). Most `pub(crate)` items and re-exports are legitimately dead until Phase 1 wires call sites. Remove these allows at the start of Phase 1, not before.
- **Use `todo!()`, never `unimplemented!()`** in stub bodies — workspace lint warns on `unimplemented` and CI's `-D warnings` turns it into an error.

## Dep version gotchas

- `rusqlite` is pinned at 0.38 (not latest) — `arti-client 0.41`'s `tor-dirmgr` transitively requires `>=0.36,<0.39`. Bumping breaks the `links = "sqlite3"` uniqueness rule. Revisit when arti bumps.
- OpenMLS triplet version numbers don't line up: `openmls = 0.8`, `openmls_traits = 0.5`, `openmls_rust_crypto = 0.5`. Don't "match" them.
- `[u8; N>32]` fields (signatures, 64-byte keys) need `#[serde(with = "serde_big_array::BigArray")]` — serde derive doesn't cover arrays longer than 32.
- `ciborium::ser::Error` / `de::Error` are generic over `W::Error`/`R::Error`; `#[from]` is fragile. Use `.map_err(|e| CoreError::CborEncode(e.to_string()))`.

## Commands

**Cargo isn't on system PATH** — prefix with `. "$HOME/.cargo/env" &&` or add `~/.cargo/bin` to your shell. rustup was installed at the user level during bootstrap.

Scaffold is in place and builds clean (`cargo build` / `cargo clippy -D warnings` / `cargo test` all green as of bootstrap).

```bash
cargo build                          # build all crates
cargo test                           # run all tests
cargo test -p core identity          # run tests for one module
cargo test --test handshake          # run a single integration test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo deny check                     # licenses, advisories, sources (config in deny.toml)
cargo audit                          # advisory scan

# CLI (once implemented)
cargo run -p cli -- init             # generate identity + seed phrase
cargo run -p cli -- restore <seed>   # rebuild identity from seed
cargo run -p cli -- daemon           # start Tor, publish onion, accept connections
cargo run -p cli -- invite [--qr]
cargo run -p cli -- add <link>
cargo run -p cli -- send <contact> <text>
```

CI (`ci.yml`) runs fmt + clippy + test on `ubuntu-latest` only, plus a
dedicated `ui` job (also `ubuntu-latest`) for the Tauri 2 + SvelteKit crate and
a `cargo-deny` job. Windows-only compile/lint errors are caught by the local
Windows build loop on myggdesk before a release is tagged (the ubuntu-only CI
gap is covered by the workflow, not an extra runner). macOS is not built or
tested anywhere — no Mac hardware — though the macOS runtime seams remain in the
code. `release.yml` builds and signs on `ubuntu-latest` + `windows-latest` at
tag time, shipping `.deb`/AppImage/`.msi` (no prebuilt `.dmg`).

## When extending the design

- Protocol-level changes (frame types, invite fields, handshake binding, MLS ciphersuite) need an ADR under `docs/adr/` with rationale before code.
- New dependencies need justification in the PR and must pass `cargo-deny` (license allowlist, no git deps, no banned crates).
- Every PR touching crypto, protocol, or auth requires a second reviewer.
