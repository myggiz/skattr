# Contributing to Skattr

Thanks for your interest in Skattr — a Rust, desktop-first, metadata-resistant
P2P encrypted messenger (all traffic over Tor v3 onion services). This document
covers how to propose changes and the bar a pull request has to clear.

Skattr is owned by Myggiz AB (Sweden) and is pre-1.0 software. Please read the
[threat model](docs/skattr-design.md) and [`SECURITY.md`](SECURITY.md) before
relying on it for anything sensitive.

## Reporting security issues

**Do not open a public issue for a security problem.** Follow
[`SECURITY.md`](SECURITY.md): email `security@myggiz.net`. See that file for
scope and what to expect.

## Reporting bugs and proposing features

The backlog lives in **GitHub issues**. Before opening a new one, search existing
issues (including closed) and the known-limitation list in `SECURITY.md` — several
absences (no multi-member groups, no metadata padding, direct-only first-contact
Welcome, advisory onion rotation) are documented, deliberate v1.0 limitations, not
bugs.

A good issue carries: what you observed vs. expected, `file:line` if you have it,
repro steps, and your OS/version.

## Sign your commits — Developer Certificate of Origin (DCO)

Skattr uses the [Developer Certificate of Origin](DCO) instead of a CLA. Every
commit must be signed off, certifying you wrote the change (or have the right to
submit it) under the project's license. Add the trailer with `-s`:

```bash
git commit -s -m "your message"
```

This appends a line to the commit message:

```
Signed-off-by: Your Name <your.email@example.com>
```

Use your real name and a reachable email. PRs whose commits lack a valid
`Signed-off-by` will not be merged. To fix an existing branch, amend
(`git commit --amend -s`) or rebase with `git rebase --signoff`.

## Licensing of contributions

Inbound = outbound. By contributing you agree your work is licensed under the
same terms as the code you touch:

- `core`, `cli`, `tests` — **GPL-3.0-or-later**
- `mailbox` — **AGPL-3.0-or-later**

Every source file must carry its SPDX license header, matching the crate
(`// SPDX-License-Identifier: GPL-3.0-or-later` or `AGPL-3.0-or-later`). See
[`LICENSE-GPL3`](LICENSE-GPL3), [`LICENSE-AGPL3`](LICENSE-AGPL3), and
`docs/adr/0001-license.md` for the rationale.

## Development workflow

- **Never commit to `master` directly.** Branch, then open a pull request.
- One logical change per PR. Keep the diff focused — bias to the smallest change
  that works; don't refactor or rename code you weren't asked about in the same PR.
- Reference the issue you resolve (`Closes #NN`).

Cargo isn't assumed on `PATH`; source your rustup env first if needed
(`. "$HOME/.cargo/env"`).

### The done-gate (a PR must pass all of these)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude skattr-ui --all-targets --features test-harness -- -D warnings
cargo test
cargo clippy -p skattr-ui --all-targets -- -D warnings
cargo deny check
```

For the UI crate (`crates/ui/src-svelte`):

```bash
pnpm check          # svelte-check, 0 errors / 0 warnings
pnpm exec vitest run
```

CI runs on GitHub Actions; the same commands above are the authoritative local
gate. `clippy -D warnings` being clean is the bar — warnings are failures.

## Coding standards

**Rust**

- Newtypes over primitives; model states as enums, not bool flags.
- **No `unwrap()` / `expect()` in library code** — use `?` and typed errors
  (`thiserror` in libs, `anyhow` in binaries). Errors are our types, not a
  vendor's.
- Test-first: the test must fail before the fix.
- All secret material zeroizes (`Zeroizing` / `ZeroizeOnDrop`) — no bare
  `[u8; 32]` secrets left on the stack.
- **No custom crypto.** Use the libraries and patterns the design doc specifies;
  no hand-rolled AEAD, Noise tweaks, or MLS changes.
- Functional core / imperative shell: keep I/O, clock, randomness, and env reads
  out of logic — take them as parameters, wire concretes up in `main`.

**TypeScript**

- `strict`; no `any`, no `!`, no `ts-ignore`. Branded types for IDs; parse at the
  boundary. Discriminated unions over optional-field soup. `tsc --noEmit` +
  eslint are the done-gate.

**Everything**

- Leave no dead code or TODO stubs. Fail loudly rather than adding defensive
  handling for cases that can't happen. If a rule makes the code worse here, say
  so in the PR and write the simpler version.

## Changes that need extra review

- **Protocol / crypto / auth changes** (frame types, invite fields, handshake
  binding, MLS ciphersuite, IPC auth) require:
  - an **ADR** under `docs/adr/` with rationale, filed *before* the code, and
  - a **second reviewer**.
- **New dependencies** need justification in the PR and must pass `cargo-deny`
  (license allowlist, no git deps, no banned crates).

## Locked decisions

Some technical choices (edition/toolchain, async runtime, Tor via Arti, the Noise
pattern, MLS ciphersuite, crypto libraries, wire serialization, storage, the
transport↔MLS binding) are deliberately locked. See "Locked technical decisions"
in the design doc before proposing a change to any of them — reversing one has
cascading consequences and needs an ADR.

## Questions

Open a GitHub Discussion or a non-security issue. For anything touching a
vulnerability, use `SECURITY.md`.
