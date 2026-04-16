# Contributing to Skattr

Thanks for your interest. Skattr is a security-sensitive project, so the bar for changes is high. This document covers how to build, test, and get a change merged.

## Before you start

- Read [`docs/skattr-design.md`](docs/skattr-design.md) end-to-end.
- Skim [`docs/skattr-implementation-plan.md`](docs/skattr-implementation-plan.md) to locate what phase you'd be contributing to.
- For crypto-, protocol-, or auth-related changes: open an issue or draft an ADR (`docs/adr/`) *before* writing code. These changes always need a second reviewer.

## Development setup

Install Rust stable (the toolchain is pinned via `rust-toolchain.toml`). On Linux you also need a C toolchain, `pkg-config`, and OpenSSL headers.

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check       # licenses, advisories, bans, sources
```

Run the CLI locally:

```bash
cargo run -p skattr-cli -- init
cargo run -p skattr-cli -- daemon
```

## Coding standards

- **Rust edition:** 2021, stable toolchain.
- **No `unsafe` in this workspace** — the `forbid(unsafe_code)` lint is workspace-wide.
- **No `unwrap()` / `expect()` in library code.** Use `?` and typed errors (`CoreError`). Binaries may use `anyhow` at the top level.
- **No panics on adversary-controlled input.** Parsers must return errors, never panic.
- **Secrets zeroize.** Any type holding key material, seed bytes, or passphrase-derived keys must implement or derive `ZeroizeOnDrop`, or wrap its fields in `Zeroizing`.
- **No custom crypto.** No hand-rolled AEADs, no "slightly modified" Noise patterns, no MLS tweaks. Use the crates called out in the design doc.
- **Every `.rs` file has a license header.** GPLv3 for `core`/`cli`/`tests`, AGPLv3 for `mailbox`. See existing files for the exact header.
- **Public items have doc comments.** `missing_docs` is `warn` at workspace level.
- **Logging hygiene.** Never log pubkeys, onion addresses, or message contents at `info` or higher. Gate verbose output behind `debug!`/`trace!`.

## Commits

- Small, focused commits. Squashing happens at merge.
- Commit subject: imperative mood, ≤72 chars (e.g. `transport: reject handshake frames over 64 KiB`).
- Sign your commits (`git commit -S`). Unsigned commits on protected branches will be rejected post-Phase-0.

## Pull requests

1. Open against `main` from a feature branch.
2. Link the related issue or ADR.
3. Explain *why* in the description, not just *what* — reviewers need the rationale.
4. If you added a dependency, justify it in the PR description (deny.toml enforces source/license/ban rules).
5. CI must be green: `fmt`, `clippy -D warnings`, `test`, `cargo-deny`, `cargo-audit`.
6. Crypto/protocol/auth PRs require review from **two** maintainers.

## Reporting bugs

Public issue tracker for non-security bugs. Security vulnerabilities → [`SECURITY.md`](SECURITY.md).

## License of contributions

By contributing you agree your code is released under the same license as the crate it lands in (GPLv3 for most crates, AGPLv3 for `mailbox`).
