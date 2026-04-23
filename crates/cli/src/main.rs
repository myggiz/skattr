// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `skattr` — the command-line client.
//!
//! All subcommands are thin wrappers over `skattr_core::Daemon`. In
//! Phase 0 each subcommand acknowledges the request and prints a
//! placeholder message; implementations land in Phase 1.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use skattr_core::daemon::Config;
use skattr_core::identity::{IdentityKey, Mnemonic, Seed, Vault};

/// `skattr` command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "skattr",
    version,
    about = "Skattr: metadata-resistant encrypted messaging over Tor.",
    long_about = None,
)]
struct Cli {
    /// Path to config file. Defaults to `~/.config/skattr/config.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the data directory (vault + daemon state). Defaults to
    /// the XDG data dir.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// JSON output (for scripting).
    #[arg(long, global = true)]
    json: bool,

    /// Read the vault passphrase from FILE (one passphrase, optional
    /// trailing newline). Overridden by `$SKATTR_PASSPHRASE_FILE`.
    #[arg(long, value_name = "FILE", global = true)]
    passphrase_file: Option<PathBuf>,

    /// Subcommand.
    #[command(subcommand)]
    cmd: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a new identity and seed phrase.
    Init,
    /// Restore an identity from a BIP39 seed phrase.
    Restore {
        /// Space-separated seed phrase (quoted).
        seed: String,
    },
    /// Export a portable backup of identity + storage + HS key to FILE.
    Backup {
        /// Destination archive path.
        file: PathBuf,
    },
    /// Restore identity + storage + HS key from a backup archive.
    RestoreBackup {
        /// BIP39 mnemonic (quoted space-separated words).
        seed: String,
        /// Source archive path.
        file: PathBuf,
    },
    /// Start the daemon (Tor bootstrap + onion publish + accept loop).
    Daemon {
        /// Detach to a background process after startup.
        #[arg(long)]
        detach: bool,
    },
    /// Generate a single-use invite link.
    Invite {
        /// Render as a QR code (requires the `qr` feature, enabled by default).
        #[arg(long)]
        qr: bool,
    },
    /// Consume an invite link to add a contact.
    Add {
        /// `skattr://invite/v1#…` URL.
        link: String,
    },
    /// List known contacts.
    Contacts,
    /// Send a text message to a contact.
    Send {
        /// Contact identifier (display name or hex prefix of identity pubkey).
        contact: String,
        /// Message body.
        text: String,
    },
    /// Tail incoming messages.
    Tail {
        /// Only from this contact.
        contact: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skattr_cli=info,skattr_core=info,warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let passphrase_file = cli_passphrase_file_or_env(&cli);
    match cli.cmd {
        Command::Init => init(cli.data_dir.as_deref()).await,
        Command::Restore { seed } => restore(&seed, cli.data_dir.as_deref()).await,
        Command::Backup { file } => backup(&file, cli.data_dir.as_deref()).await,
        Command::RestoreBackup { seed, file } => {
            restore_backup(&seed, &file, cli.data_dir.as_deref()).await
        }
        Command::Daemon { detach } => {
            daemon(detach, cli.data_dir.as_deref(), passphrase_file).await
        }
        Command::Invite { qr } => invite(qr).await,
        Command::Add { link } => add(&link).await,
        Command::Contacts => contacts().await,
        Command::Send { contact, text } => send(&contact, &text).await,
        Command::Tail { contact } => tail(contact.as_deref()).await,
    }
}

fn effective_data_dir(override_dir: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d.to_path_buf());
    }
    Ok(Config::defaults()?.data_dir)
}

async fn init(data_dir_override: Option<&std::path::Path>) -> Result<()> {
    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let pw1 = read_passphrase("Choose a passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    let seed = Seed::generate()?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    Vault::create(&vault_path, identity, pw1.as_str())?;

    let mnemonic = seed.to_mnemonic()?;
    let phrase = mnemonic.words().join(" ");

    println!();
    println!("Identity created.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    println!();
    println!("RECOVERY SEED PHRASE — write this down, store it offline:");
    println!();
    println!("  {phrase}");
    println!();
    println!("If you lose this phrase AND the vault passphrase, your identity is");
    println!("unrecoverable. We cannot reset it for you.");
    Ok(())
}

/// Source the daemon passphrase can come from.
#[derive(Debug, Clone)]
enum PassphraseSource {
    /// Prompt on `/dev/tty` with echo off.
    InteractiveTty(String),
    /// Read from a file at `path`; trim exactly one trailing newline.
    File(std::path::PathBuf),
}

fn read_passphrase(prompt: &str) -> Result<zeroize::Zeroizing<String>> {
    read_passphrase_from_source(PassphraseSource::InteractiveTty(prompt.to_string()))
}

fn read_passphrase_from_source(source: PassphraseSource) -> Result<zeroize::Zeroizing<String>> {
    match source {
        PassphraseSource::InteractiveTty(prompt) => {
            let raw = rpassword::prompt_password(prompt)
                .map_err(|e| anyhow::anyhow!("read passphrase: {e}"))?;
            Ok(zeroize::Zeroizing::new(raw))
        }
        PassphraseSource::File(path) => read_passphrase_from_file(&path),
    }
}

fn read_passphrase_from_file(path: &std::path::Path) -> Result<zeroize::Zeroizing<String>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read passphrase from {}: {e}", path.display()))?;
    // Trim exactly one trailing newline (CRLF or LF), preserve internal newlines.
    let mut pw = zeroize::Zeroizing::new(raw);
    if pw.ends_with('\n') {
        pw.pop();
        if pw.ends_with('\r') {
            pw.pop();
        }
    }
    Ok(pw)
}

fn cli_passphrase_file_or_env(cli: &Cli) -> Option<PathBuf> {
    if let Some(p) = &cli.passphrase_file {
        return Some(p.clone());
    }
    std::env::var_os("SKATTR_PASSPHRASE_FILE").map(PathBuf::from)
}

async fn restore(seed_phrase: &str, data_dir_override: Option<&std::path::Path>) -> Result<()> {
    use anyhow::Context;

    let data_dir = effective_data_dir(data_dir_override)?;
    std::fs::create_dir_all(&data_dir)?;
    let vault_path = data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    // Parse the phrase through a Zeroizing copy so our local String
    // does not linger. (The clap-owned argv slice is still exposed via
    // /proc/<pid>/cmdline — users should avoid passing secrets on the
    // command line; this is documented in the README.)
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&owned)
    };
    let seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&seed)?;
    let pubkey_hex = identity.public().to_hex();

    let pw1 = read_passphrase("Choose a new vault passphrase: ")?;
    let pw2 = read_passphrase("Confirm passphrase: ")?;
    if *pw1 != *pw2 {
        anyhow::bail!("passphrases do not match");
    }

    Vault::create(&vault_path, identity, pw1.as_str())?;

    println!();
    println!("Identity restored.");
    println!("  public key: {pubkey_hex}");
    println!("  vault:      {}", vault_path.display());
    Ok(())
}

async fn backup(
    out_file: &std::path::Path,
    data_dir_override: Option<&std::path::Path>,
) -> Result<()> {
    use skattr_core::daemon::backup::export_backup;
    use skattr_core::identity::derive::derive_storage_seed;

    let data_dir = effective_data_dir(data_dir_override)?;
    let vault_path = data_dir.join("identity.vault");
    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; nothing to back up",
            vault_path.display()
        );
    }

    let pw = read_passphrase("Vault passphrase: ")?;
    let (_vault, identity) = Vault::open(&vault_path, pw.as_str())?;
    let seed = derive_storage_seed(identity)?;

    export_backup(&data_dir, out_file, &seed)?;
    println!("Backup written to {}", out_file.display());
    Ok(())
}

async fn restore_backup(
    seed_phrase: &str,
    archive_file: &std::path::Path,
    data_dir_override: Option<&std::path::Path>,
) -> Result<()> {
    use anyhow::Context;
    use skattr_core::daemon::backup::import_backup;
    use skattr_core::identity::derive::derive_storage_seed;

    let data_dir = effective_data_dir(data_dir_override)?;

    // Parse the mnemonic through a Zeroizing copy.
    let mnemonic = {
        let owned = zeroize::Zeroizing::new(seed_phrase.to_string());
        Mnemonic::parse(&owned)
    };
    let identity_seed = Seed::from_mnemonic(&mnemonic)
        .context("invalid seed phrase (check word list and checksum)")?;
    let identity = IdentityKey::from_seed(&identity_seed)?;
    let storage_seed = derive_storage_seed(identity)?;

    import_backup(archive_file, &data_dir, &storage_seed)?;
    println!("Restored from {}", archive_file.display());
    println!("Data at: {}", data_dir.display());
    println!("Run `skattr daemon` to bring the identity online.");
    Ok(())
}

async fn daemon(
    detach: bool,
    data_dir_override: Option<&std::path::Path>,
    passphrase_file: Option<PathBuf>,
) -> Result<()> {
    use skattr_core::daemon::{Config, Daemon};

    if detach {
        anyhow::bail!("--detach is not yet supported in Phase 1.F");
    }

    let mut config = Config::defaults()?;
    if let Some(override_dir) = data_dir_override {
        config.data_dir = override_dir.to_path_buf();
    }
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

    if !vault_path.exists() {
        anyhow::bail!(
            "no identity vault at {}; run `skattr init` first",
            vault_path.display()
        );
    }

    let pw = match passphrase_file {
        Some(path) => read_passphrase_from_source(PassphraseSource::File(path))?,
        None => read_passphrase("Vault passphrase: ")?,
    };

    println!("Bootstrapping Tor\u{2026}");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let shutdown_fut = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    // Move the Zeroizing<String> passphrase and config by value into the
    // spawned task — they drop (and wipe) when Daemon::run returns.
    let data_dir_owned = config.data_dir.clone();
    let config_owned = config.clone();
    let daemon_fut = tokio::spawn(async move {
        Daemon::run(&data_dir_owned, &pw, config_owned, ready_tx, shutdown_fut).await
    });

    // Wait for the daemon to signal readiness.
    let ready = ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("daemon exited before becoming ready"))?;
    println!();
    println!("Listening on: {}:1", ready.onion);
    println!("IPC socket:   {}", ready.ipc_socket.display());
    println!("Ctrl-C to shut down.");

    // Block until the daemon future returns (SIGINT + graceful shutdown).
    daemon_fut
        .await
        .map_err(|e| anyhow::anyhow!("daemon join: {e}"))??;

    println!();
    println!("Shutdown complete.");
    Ok(())
}

async fn invite(_qr: bool) -> Result<()> {
    println!("skattr invite: not yet implemented.");
    Ok(())
}

async fn add(_link: &str) -> Result<()> {
    println!("skattr add: not yet implemented.");
    Ok(())
}

async fn contacts() -> Result<()> {
    println!("skattr contacts: not yet implemented.");
    Ok(())
}

async fn send(_contact: &str, _text: &str) -> Result<()> {
    println!("skattr send: not yet implemented.");
    Ok(())
}

async fn tail(_contact: Option<&str>) -> Result<()> {
    println!("skattr tail: not yet implemented.");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_from_file_trims_single_trailing_newline() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "secret-pw").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "secret-pw");
    }

    #[test]
    fn read_from_file_preserves_internal_newlines() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "line1\nline2\n").unwrap();
        let pw = read_passphrase_from_file(tmp.path()).unwrap();
        assert_eq!(pw.as_str(), "line1\nline2");
    }

    #[test]
    fn read_from_missing_file_returns_error() {
        let err = read_passphrase_from_file(std::path::Path::new("/does/not/exist"))
            .expect_err("missing file must error");
        assert!(err.to_string().contains("/does/not/exist"));
    }
}
