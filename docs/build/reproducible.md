# Reproducible-build recipe

Phase 2.G's reproducibility goal is **inputs are pinned and the
recipe is documented**, not byte-identical output. Phase 4 closes
the byte-identical claim.

## Recipe

```bash
# 1. Use the pinned toolchain (rust-toolchain.toml does this
#    automatically when a contemporary rustup is installed).
rustup show

# 2. Set SOURCE_DATE_EPOCH from the commit's timestamp.
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)

# 3. (Linux only) drop the build-id so binaries don't bake in
#    a timestamp-derived value.
export RUSTFLAGS="-C link-arg=-Wl,--build-id=none"

# 4. Install the pinned tauri-cli.
cargo install tauri-cli --version '=2.11.0' --locked

# 5. Build the frontend deterministically.
pnpm --dir crates/ui/src-svelte install --frozen-lockfile
pnpm --dir crates/ui/src-svelte build

# 6. Build the Tauri bundles.
cargo tauri build
```

## Pinned versions

| Component         | Version pin                           | Lock location                       |
|-------------------|---------------------------------------|-------------------------------------|
| rustc             | exact stable (e.g. 1.84.1)            | `rust-toolchain.toml` `version`     |
| Tauri (Rust)      | `=2.11.0`                             | `crates/ui/Cargo.toml`              |
| `tauri-cli`       | `=2.11.0`                             | `cargo install …` step              |
| Tauri (JS)        | `2.0.0` (exact)                       | `crates/ui/src-svelte/package.json` |
| Node              | `20.x` (LTS)                          | `.github/workflows/release.yml`     |
| pnpm              | `10`                                  | `package.json` `packageManager`     |
| Rust deps         | per `Cargo.lock`                      | committed                           |
| JS deps           | per `pnpm-lock.yaml`                  | committed                           |

## Caveats

- **WebKit / WKWebView versions are platform-supplied** and not
  pinned by this recipe. Upgrading WebKitGTK on the build host
  changes the runtime behaviour even if the bundle is bit-identical.
- **GLIBC** (Linux): the build host's glibc minor version sets the
  AppImage's effective floor.
- **macOS SDK**: the bundle's `LC_BUILD_VERSION` reflects the
  Xcode SDK version of the build host. Two builds on different
  Xcode versions will not be byte-identical.
- **Cargo.lock + pnpm-lock.yaml are CI-enforced** (already true
  from earlier phases) — every PR that drifts the lockfiles fails
  CI.

## Phase 4 follow-up

Phase 4 will:

- Pin a containerised build environment (Nix flake or Docker
  image with frozen system libraries).
- Verify byte-identical reproducibility across two independent
  builds.
- Publish a reproducer recipe alongside each release.

For Phase 2.G, the recipe above is the contract.
