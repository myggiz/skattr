# Skattr Operations

> Target audience: contributors + operators running the dev stack.
> End-users should read the README instead.

## Prerequisites

- Rust stable (pinned in `rust-toolchain.toml`). Install via
  [rustup](https://rustup.rs). If `cargo` is not on your PATH, source
  `~/.cargo/env`.
- A C compiler and `pkg-config` — required transitively by Arti's
  dependencies on some Linux distributions.
- `git`.
- For the real-Tor integration test: internet access on the first run
  (~30 s consensus download per daemon), Tor directory authorities
  reachable on port 9030/443.
- For the fuzz harness: nightly Rust (`rustup toolchain install
  nightly --profile minimal`) and `cargo-fuzz` (`cargo install
  cargo-fuzz`).

## One-time setup

```bash
git clone https://github.com/myggiz/skattr
cd skattr
cargo build --workspace
```

First build is slow (~10 min on a fast laptop; Arti pulls ~100
crates). Subsequent incremental builds are fast.

## Running the test suite

```bash
# Fast unit + integration tests. Runs in ~30 s.
cargo test --workspace --release

# Slow tests that hit the Tor network (two daemons echo bytes).
# Requires real Tor connectivity. Takes 3-10 min.
cargo test -p skattr-tests --release -- --ignored

# Format check + clippy (what CI runs).
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Running the daemon locally

```bash
# Step 1: generate an identity + a passphrase-locked vault.
# This writes identity.vault under ~/.local/share/skattr/.
# Record the 24-word seed phrase printed on stdout — it's your
# only recovery path.
cargo run -p skattr-cli -- init

# Step 2: start the daemon.
cargo run -p skattr-cli -- daemon
```

The first daemon start bootstraps Arti (~30-90 s on a fresh
`state_dir`) and publishes a v3 onion service. You'll see:

```
Bootstrapping Tor…
Tor ready. Publishing onion service…

Listening on: abcdef...onion:1
Ctrl-C to shut down.
```

The `.onion` address is derived from your identity seed, so restoring
from the BIP39 mnemonic reproduces the same address (see
[recovery](#backup-and-recovery)).

### Running two daemons on the same machine

For testing, use `--data-dir` to point each daemon at a different
directory:

```bash
TMP1=$(mktemp -d)
TMP2=$(mktemp -d)
cargo run -p skattr-cli -- --data-dir "$TMP1" init  # daemon A
cargo run -p skattr-cli -- --data-dir "$TMP2" init  # daemon B
cargo run -p skattr-cli -- --data-dir "$TMP1" daemon  # in one terminal
cargo run -p skattr-cli -- --data-dir "$TMP2" daemon  # in another
```

Arti requires `$TMP1` and `$TMP2` to be mode `0700` — if you created
them with a permissive umask, run `chmod 700 $TMP1 $TMP2` first.

## Backup and recovery

### Backup

```bash
cargo run -p skattr-cli -- backup /path/to/backup.age
```

You'll be prompted for the vault passphrase. The archive is a
tar.gz of:

- `identity.vault` (passphrase-encrypted)
- `hs.key.age` (seed-encrypted)
- `skattr.sqlite.age` (seed-encrypted)

with an outer age layer keyed by `HKDF(seed, "skattr-backup-v1")`.
The archive is safe to store on untrusted media; the inner layers
mean even the BIP39 seed alone is not enough to open it without the
vault passphrase.

### Recovery from the BIP39 seed alone (identity only)

```bash
cargo run -p skattr-cli -- restore "word1 word2 ... word24"
```

This rebuilds `identity.vault` under a fresh passphrase you choose.
The seed phrase is sufficient to recover the identity AND the
`.onion` address AND the HS signing key. It does NOT recover the
message history — you'll have a fresh SQLite database.

### Recovery from a backup archive (full state)

```bash
cargo run -p skattr-cli -- restore-backup "word1 word2 ... word24" /path/to/backup.age
```

This extracts all three inner files into your `--data-dir`.
Subsequent `skattr daemon` picks up the restored identity, the same
`.onion`, and the same message history.

## Known operational issues

### Daemon killed ungracefully → plaintext `skattr.sqlite` on disk

If the daemon process dies without a clean `Ctrl-C` (e.g., `SIGKILL`,
crash), the plaintext working file `<data_dir>/skattr.sqlite` remains
on disk. This is by design: next startup re-opens it directly and
continues. Drawback: at-rest encryption is effectively disabled
until the next clean shutdown. Phase 1 will add a sync-on-checkpoint
path.

Manual recovery: if you want to re-encrypt immediately, the simplest
route is to run `skattr daemon` once more and let it exit cleanly.

### Arti bootstrap fails with "filesystem permissions"

Arti 0.41 refuses to open a `state_dir` that's group- or
world-readable. If you see:

```
arti client: tor: problem with filesystem permissions
```

Fix it with:

```bash
chmod 700 /path/to/data_dir
```

`skattr daemon` will auto-create its subdirectories with the right
mode going forward.

### Forgot the vault passphrase

Without the passphrase there is no way to decrypt `identity.vault`.
Use `skattr restore <24-word seed phrase>` to rebuild the vault
under a new passphrase. You keep the identity and the `.onion`
address; you lose only the passphrase itself.

### Forgot the seed phrase AND the vault passphrase

The identity is unrecoverable. By design. There is no key-recovery
server.

## Reaching dev infrastructure

- **Issue tracker:** GitHub issues on this repo (once public).
  Security issues route privately per `SECURITY.md`.
- **Fuzz corpus:** local under `crates/core/fuzz/corpus/` when you
  run `cargo +nightly fuzz run`.
- **ADRs:** `docs/adr/` — append a new numbered file for any
  protocol-layer, crypto, or storage decision.

## Phase-0 completion checklist

If you're reading this at the end of Phase 0, the following should
all be true. Run through them as a sanity sweep after any significant
change to the transport or storage layers:

- [ ] `cargo test --workspace --release` passes with 77+ tests.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo fmt --all --check` clean.
- [ ] `cd crates/core && cargo +nightly fuzz build vault_parser` succeeds.
- [ ] `cargo test -p skattr-tests --release -- --ignored` passes on a
  network-connected machine (3-10 min).
- [ ] `skattr --help` prints all subcommands: init, restore, daemon,
  invite, add, contacts, send, tail, backup, restore-backup.
- [ ] `skattr init` → record phrase → `skattr daemon` → see `.onion` →
  Ctrl-C shuts down cleanly.
- [ ] `skattr backup` and `skattr restore-backup` round-trip against a
  clean data_dir.
