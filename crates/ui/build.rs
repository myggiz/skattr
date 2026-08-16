// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz B.V.

//! Cargo build script — invokes `tauri_build` to bake the Tauri 2
//! context (icons, capabilities, frontend dist path) at compile time.

fn main() {
    warn_if_release_build_is_dev_mode();
    tauri_build::build();
}

/// Warn when a *release* build would silently produce a dev-mode binary (#183).
///
/// Tauri decides dev-vs-production from the `custom-protocol` cargo feature and
/// publishes the answer as `cargo:dev=` from its own build script; because
/// `tauri` declares `links = "Tauri"`, cargo hands that to us as
/// `DEP_TAURI_DEV`. `tauri_build::build()` reads the very same variable, so it
/// is the authoritative signal rather than a guess.
///
/// Keyed on `DEP_TAURI_DEV` and not on our own `CARGO_FEATURE_CUSTOM_PROTOCOL`:
/// enabling the upstream feature directly (`--features tauri/custom-protocol`)
/// produces a correct binary without setting ours, and warning there would be
/// a false alarm.
///
/// Without this, `cargo build -p skattr-ui --release` compiles, lints, reports
/// the right version, and yields a binary that loads `devUrl`
/// (`http://localhost:1420`) and cannot render the UI — with nothing in the
/// build output to say so.
fn warn_if_release_build_is_dev_mode() {
    let dev = std::env::var("DEP_TAURI_DEV").as_deref() == Ok("true");
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    if dev && release {
        println!(
            "cargo:warning=skattr-ui: release build WITHOUT the `custom-protocol` feature. \
             This binary will load devUrl (http://localhost:1420) and cannot render the UI. \
             Build it with `cargo tauri build` (from crates/ui), or add \
             `--features custom-protocol`."
        );
    }
}
