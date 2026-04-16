# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Phase 0 workspace scaffold: `core`, `mailbox`, `cli`, `tests` crates.
- Module tree for protocol, transport, MLS, storage, delivery, daemon.
- Initial SQL migration (`0001_init.sql`).
- CLI subcommands (stubbed): `init`, `restore`, `daemon`, `invite`, `add`, `send`, `contacts`.
- Architecture Decision Records 0001–0003.
- `cargo-deny` and CI matrix across Linux/macOS/Windows.

[Unreleased]: https://github.com/myggiz/skattr/compare/main...HEAD
