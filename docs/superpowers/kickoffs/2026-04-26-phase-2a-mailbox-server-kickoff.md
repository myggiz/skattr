# Phase 2.A — Mailbox server kickoff prompt

> **Usage:** Paste the fenced block below as the first message of a
> fresh Claude Code session. Keep the surrounding meta-text out of
> the paste — only the fenced block is the prompt itself.

---

```
Phase 2 decomposition just merged (caf9755) — see
`docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` for
the umbrella spec.

Phase 2.A ships the standalone mailbox server: a separate, AGPLv3-
licensed binary (`crates/mailbox/`) that holds encrypted deposits for
offline recipients. Wire types live in a new `core::mailbox::protocol`
module shared with 2.B (the client).

Please start by invoking `superpowers:brainstorming` to refine 2.A's
internals. Topics worth pinning down:

- Crate split: binary + library so soak tests drive the library
  directly, or single-binary?
- Storage: plain SQLite (no `age` — mailbox sees only ciphertext)
  vs. encrypted-at-rest. Trade-offs around operator backup ergonomics.
- Wire-protocol message shapes: confirm the four frames (DEPOSIT,
  CHALLENGE, FETCH, DELETE), their CBOR bodies, and error variants.
- Caps + TTL: defaults from the decomposition spec are 1 MiB/deposit,
  TTL clamp [1h, 30 days], 256 MiB/recipient, 30 deposits/min/circuit
  + 6 fetches/min/circuit. Confirm or adjust.
- Operational artefacts: systemd unit, Dockerfile (distroless),
  healthcheck shape, logging redaction policy.
- Testing: unit + property + fuzz + 24h soak. What's the minimum bar
  for "protocol frozen for 2.B"?

## Context

- `docs/superpowers/specs/2026-04-26-phase-2-ui-decomposition.md` —
  the umbrella; 2.A's sketch is in §"Sub-project sketches → 2.A".
- `docs/skattr-implementation-plan.md` Phase 2 §Workstream 2.A — the
  original detailed task list.
- `docs/skattr-design.md` — protocol-level mailbox semantics.
- `docs/skattr-deep-dives.md` Part 3 — mailbox wire protocol design.
- `core::transport` already provides Noise_XK + frame codec; reuse it,
  do not introduce a parallel wire format.
- CLAUDE.md locked decisions remain binding. Note that `mailbox` crate
  is **AGPLv3** (not GPLv3 — distinct from `core`/`cli`/`tests`).

## Locked from the decomposition spec (do not relitigate)

- 2.A is mailbox-server-only; the client lives in 2.B.
- Wire types live in `core::mailbox::protocol` (pub) for cross-crate
  use by 2.B's client.
- Reuse `core::transport`'s Noise_XK + frame codec — no parallel wire.
- Operator-configurable caps with the defaults listed above.
- AGPLv3 header on every `crates/mailbox/**/*.rs`.
- No identity pubkeys or full hashes in any log line above `debug`.

## After brainstorming

- `superpowers:writing-plans` to author the implementation plan.
- `superpowers:using-git-worktrees` to branch off master onto a
  `phase-2a-mailbox-server` branch.
- `superpowers:test-driven-development` + `superpowers:subagent-
  driven-development` to execute.
- `superpowers:verification-before-completion` before the merge PR.

## Out of scope for 2.A

- Client-side mailbox use (2.B).
- ContactCard rotation (2.B).
- UI surfaces for mailbox CRUD (2.F; 2.C ships only stub IPC).
- A "use this mailbox" public directory (Phase 5+).
```
