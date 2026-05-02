// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Pre-daemon Tauri commands. These are the only Tauri commands that
//! run before `Daemon::run` is spawned. Three commands total:
//! `vault_exists`, `identity_init`, `vault_unlock`. The lint test in
//! this module enforces the cap.

use serde::{Deserialize, Serialize};

use crate::daemon::AppState;

#[tauri::command]
pub async fn vault_exists(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    Ok(data_dir.join("identity.vault").exists())
}

#[derive(Debug, Deserialize)]
pub struct IdentityInitArgs {
    pub passphrase: String,
    pub mnemonic: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IdentityInitResult {
    pub mnemonic: String,
}

#[tauri::command]
pub async fn identity_init(
    state: tauri::State<'_, AppState>,
    args: IdentityInitArgs,
) -> Result<IdentityInitResult, String> {
    use skattr_core::identity::{IdentityKey, Mnemonic, Seed, Vault};
    use zeroize::Zeroizing;

    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let vault_path = data_dir.join("identity.vault");
    if vault_path.exists() {
        return Err("vault already exists".to_string());
    }

    let seed = match args.mnemonic.as_deref() {
        Some(words) => {
            let parsed: Vec<String> = words.split_whitespace().map(str::to_string).collect();
            let m = Mnemonic::from_words(parsed);
            Seed::from_mnemonic(&m).map_err(|e| format!("bad mnemonic: {e}"))?
        }
        None => Seed::generate().map_err(|e| format!("seed gen: {e}"))?,
    };
    let mnemonic = seed.to_mnemonic().map_err(|e| format!("mnemonic: {e}"))?;
    let key = IdentityKey::from_seed(&seed).map_err(|e| format!("key: {e}"))?;

    let pass = Zeroizing::new(args.passphrase);
    Vault::create(&vault_path, key, pass.as_str()).map_err(|e| format!("vault create: {e}"))?;

    Ok(IdentityInitResult {
        mnemonic: mnemonic.words().join(" "),
    })
}

#[derive(Debug, Deserialize)]
pub struct VaultUnlockArgs {
    pub passphrase: String,
}

#[tauri::command]
pub async fn vault_unlock(
    state: tauri::State<'_, AppState>,
    args: VaultUnlockArgs,
) -> Result<(), String> {
    use skattr_core::identity::Vault;
    let data_dir = state
        .data_dir
        .read()
        .clone()
        .ok_or_else(|| "data_dir not initialised".to_string())?;
    let vault_path = data_dir.join("identity.vault");
    if !vault_path.exists() {
        return Err("no vault to unlock".to_string());
    }
    let pass = zeroize::Zeroizing::new(args.passphrase.clone());
    let _ = Vault::open(&vault_path, pass.as_str()).map_err(|e| format!("unlock failed: {e}"))?;
    // Stash the passphrase in state for daemon::start_in_process_cmd to consume.
    *state.pending_passphrase.write() = Some(zeroize::Zeroizing::new(args.passphrase));
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Lint guard: the pre-daemon Tauri command surface is restricted
    /// to three annotations. Adding a fourth requires re-evaluating
    /// the wizard-first contract from the 2.C spec.
    #[test]
    fn bootstrap_tauri_commands_are_capped_at_three() {
        // CARGO_MANIFEST_DIR is the absolute path to crates/ui/; file!() is
        // a path relative to the workspace root that doesn't resolve from
        // an arbitrary cwd (e.g., when `cargo test` is invoked from outside
        // the worktree). Anchor against the manifest dir for portability.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bootstrap.rs");
        let src = std::fs::read_to_string(&path).unwrap();
        // Count only lines whose trimmed leading content is the
        // attribute itself (so the literal needle inside this test
        // body — embedded as a string — doesn't get counted).
        let count = src
            .lines()
            .filter(|l| l.trim_start().starts_with("#[tauri::command]"))
            .count();
        assert_eq!(
            count, 3,
            "bootstrap.rs must expose exactly 3 Tauri commands; got {count}"
        );
    }
}
