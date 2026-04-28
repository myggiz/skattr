# Phase 2.B — Mailbox client + ContactCard rotation kickoff prompt

> **Usage:** Paste the fenced block below as the first message of a
> fresh Claude Code session. Keep the surrounding meta-text out of
> the paste — only the fenced block is the prompt itself.

---

```
Phase 2.A just merged — the standalone mailbox server is in
`crates/mailbox/`, the v1 wire types are frozen in
`core::mailbox::protocol`, and ADR 0006
(`docs/adr/0006-mailbox-protocol-v1.md`) documents the freeze
including the positional-CBOR-tuple auth digest.

Phase 2.B implements the client half: a `MailboxClient` that talks
v1 to operator-run mailboxes, a `PollScheduler` that drives fetches
on an adaptive cadence, the `DeliveryHub` direct→mailbox fallback,
ContactCard v2 with embedded mailbox references, and the
RotateOnion / AddMailbox / RemoveMailbox commands. The frozen 2.A
wire surface is binding — no protocol changes; this is a pure
client-side build.

Please start by invoking `superpowers:brainstorming` to refine 2.B's
internals. Topics worth pinning down:

- MailboxClient connection lifecycle: long-lived
  `Framed<DataStream, MailboxFrameCodec>` per mailbox (cheaper, no
  per-op Tor circuit cost) vs. open-on-demand (simpler, less idle
  state). The v1 protocol has CHALLENGE per Fetch/Delete pair, but
  DEPOSIT is unauthenticated, so a single connection can serve
  multiple operations.
- PollScheduler shape: Idle 60s ↔ Active 15s with ±25% jitter (per
  decomposition). What triggers Idle→Active — local send / local
  receive / mailbox push? Per-mailbox state machine or a single
  global scheduler that fans out?
- mailboxes table schema: columns (id, onion, status, registered_at,
  last_poll_at, last_error_at, last_error_kind, …) and migration
  number (next free is 0008 in core/storage).
- Outbox extension migration: `outbox.target_kind` ('direct' |
  'mailbox') + `outbox.mailbox_id` (FK). Single migration covering
  the column adds, the FK constraint, and any backfill required for
  pre-2.B rows.
- DeliveryHub fallback policy: try direct connection for N seconds
  (default 30s), then enqueue mailbox-deposit attempts in parallel
  to each of the recipient's known mailboxes. What if the recipient
  has zero mailboxes listed (drop, queue, surface error)? What if
  all mailboxes reject (RateLimited cascade vs. give-up)?
- ContactCard v2: optional `mailboxes: Vec<MailboxRef>` field
  (additive — verifiers tolerate absence to keep 1.D-era cards
  verifying); MailboxRef shape (onion + maybe a key fingerprint?).
  Sign/verify covers any canonical-CBOR shape (1.D); confirm the
  field is included in the signature for v2 cards. Monotonic
  `version` bump on every rotation.
- RotateOnion flow: new HS key, publish v2 ContactCard via deposits
  to every contact's mailboxes in parallel, old onion stays
  listening for a configurable grace period (24h default). What
  about contacts that don't have a mailbox listed — defer rotation,
  warn, or drop?
- AddMailbox / RemoveMailbox: register-then-publish or
  publish-after-validation? What does "register" mean in v1 (the
  protocol has no REGISTER frame — first DEPOSIT/CHALLENGE is the
  binding)?
- Mailbox-deposit ACK semantics: once a deposit succeeds (DepositOk),
  is the message considered "delivered" by the sender? Or do we
  wait for a peer-originated ACK frame over direct connection
  later? Decomposition implies the latter; nail down the
  delivery-state state machine (queued → deposited → acked).
- Events: `MailboxStatusChanged { mailbox_id, status }` —
  status enum members (Reachable / Unreachable / Authenticated /
  RateLimited?). `ContactCardReceived { contact, version }` for the
  UI to refresh.
- Test plan: in-process `MailboxServer` over `tokio::io::duplex`
  for fast integration tests (matches 2.A's pattern); separate
  `#[ignore]`-gated real-Tor scenario that drives a daemon-pair +
  spawned skattr-mailbox binary.

## Context

- `docs/adr/0006-mailbox-protocol-v1.md` — frozen v1 wire surface
  this client implements against. Read first.
- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md`
  §"2.B — Mailbox client + ContactCard rotation" — sketch.
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.B — the
  original detailed task list.
- `crates/core/src/mailbox/protocol.rs` — frozen wire types.
- `crates/core/src/mailbox/{client,scheduler}.rs` — Phase 1.E-era
  stubs; replace with the real implementation.
- `crates/mailbox/src/server.rs` (and `dispatch`, `auth`, `codec`)
  — server-side counterparts to mirror the auth-string
  construction, esp. the positional CBOR tuple in `dispatch.rs`.
- `crates/core/src/contact/card.rs` — extend with the `mailboxes`
  field. Existing sign/verify cover any canonical shape (1.D).
- `crates/core/src/delivery/hub.rs` — extension point for the
  direct→mailbox fallback policy.
- `crates/core/src/storage/migrations/` — next free migration is
  0008.
- CLAUDE.md locked decisions remain binding. `core` is GPLv3;
  `core::mailbox::client` and `core::mailbox::poll` stay
  `pub(crate)` with the Daemon/IPC layer as the stable boundary.

## Locked from the 2.A merge (do not relitigate)

- v1 wire surface is frozen — every change requires v2.
- The auth digest input is a positional CBOR tuple
  (`(version, identity_pubkey, nonce[, deposit_ids])`); the client
  MUST mirror this exactly.
- DEPOSIT is unauthenticated; FETCH and DELETE require a CHALLENGE
  nonce with a 30 s TTL, single-use on successful verify.
- Recipient identity binds via `sha256(identity_pubkey)` —
  deposits use the recipient's hash; the client uses its own
  identity_pubkey + signature for fetch/delete.
- The mailbox is semi-trusted infrastructure: the client must
  assume the operator sees per-identity polling patterns and
  deposit timestamps, just not contents.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2b-mailbox-client` branch.
- `superpowers:test-driven-development` +
  `superpowers:subagent-driven-development` to execute.
- `superpowers:verification-before-completion` before the merge PR.

## Out of scope for 2.B

- UI surfaces for mailbox CRUD (2.F; 2.C ships only the IPC stubs).
- Public "use this mailbox" directory (Phase 5+).
- Cover-traffic polling (Phase 4).
- Multi-member groups (Phase 3).
- Federated mailboxes (explicitly off the design table per 2.A's
  ADR).
- Wire-protocol changes — anything that needs a new frame byte or
  a typed-field shape change is `MAILBOX_PROTOCOL_V2` and a
  separate spec.
```
