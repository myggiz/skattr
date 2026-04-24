# Phase 2 — UI (Tauri 2 + SvelteKit) kickoff prompt

> **Usage:** Paste the fenced block below as the first message of a
> fresh Claude Code session. Keep the surrounding meta-text out of
> the paste — only the fenced block is the prompt itself.

---

```
We just merged Phase 1.H (hardening) to master. All 11 items from the
Phase 1.G review threads are closed — correctness (envelope_id
uniqueness, transactional send/receive), error taxonomy (six subsystem
sub-enums), IPC polish (row_id, contact_for_group), and hygiene.

Phase 2 ships the desktop UI: Tauri 2 + SvelteKit. The design doc
calls out this is the first user-facing surface; the CLI stays
available for power users and automation.

Please start by invoking `superpowers:brainstorming` to explore scope
— there is a lot of ground here. At minimum think through:

- IPC surface coverage: every IPC command the UI needs vs. what the
  daemon exposes today. Any gaps (bulk operations, pagination, search
  scoping, event-stream filtering)?
- Tauri + SvelteKit bootstrap: what's the smallest surface that proves
  the IPC wiring — probably "show contacts list, render one
  conversation, subscribe to MessageReceived events."
- Phase 2 decomposition: one big plan, or sub-phases (2.A bootstrap,
  2.B conversation view, 2.C invite/contact flow, 2.D settings)?
- Design language: skattr's identity is privacy-forward; the UI should
  avoid tracker-y patterns (no analytics, no external fonts/CDNs,
  no embedded images from outside the app bundle).
- Offline-first behavior when daemon is down.

## Context

- `docs/skattr-implementation-plan.md` Phase 2 section has the
  original decomposition.
- `docs/superpowers/specs/2026-04-24-phase-1h-hardening-design.md`
  closes 1.H with the IPC surface listed there; assume stable.
- `crates/ui/` is reserved per CLAUDE.md — Phase 2 scaffolds it.
- CLAUDE.md locked decisions remain binding: GPLv3 on every .rs,
  no unwrap/expect in non-test, Tauri 2 + SvelteKit, Tokio runtime,
  `tracing` for logs.

## After brainstorming

- `superpowers:writing-plans` for the per-task plan (likely one plan
  per sub-phase if 2 decomposes).
- `superpowers:using-git-worktrees` to branch off master.
- `superpowers:subagent-driven-development` to execute.

Questions before you start brainstorming: is the UI scope locked to
desktop (Tauri 2), or is there interest in a web/mobile variant?
```
