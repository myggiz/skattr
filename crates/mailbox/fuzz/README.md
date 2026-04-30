# skattr-mailbox-fuzz

cargo-fuzz harness for the mailbox protocol decoder and dispatch loop.

## Local run

Requires nightly Rust:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
cd crates/mailbox/fuzz
cargo +nightly fuzz run frame_decode -- -max_total_time=3600
cargo +nightly fuzz run dispatch     -- -max_total_time=3600
```

Findings (if any) land in `artifacts/`; commit reproducer files
under `corpus/` and add a regression test in
`crates/mailbox/tests/adversarial_codec.rs`.

The Phase 2.A freeze bar requires both targets to run for >= 1 hour
locally with no findings before the merge PR.
