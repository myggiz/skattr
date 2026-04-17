// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! `skattr` — the command-line client.
//!
//! All subcommands are thin wrappers over `skattr_core::Daemon`. In
//! Phase 0 each subcommand acknowledges the request and prints a
//! placeholder message; implementations land in Phase 1.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::io::{self, BufRead, Write};
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

    /// JSON output (for scripting).
    #[arg(long, global = true)]
    json: bool,

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
    match cli.cmd {
        Command::Init => init().await,
        Command::Restore { seed } => restore(&seed).await,
        Command::Daemon { detach } => daemon(detach).await,
        Command::Invite { qr } => invite(qr).await,
        Command::Add { link } => add(&link).await,
        Command::Contacts => contacts().await,
        Command::Send { contact, text } => send(&contact, &text).await,
        Command::Tail { contact } => tail(contact.as_deref()).await,
    }
}

async fn init() -> Result<()> {
    let config = Config::defaults()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

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

fn read_passphrase(prompt: &str) -> Result<zeroize::Zeroizing<String>> {
    // TODO(phase-2): use rpassword to suppress terminal echo.
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = zeroize::Zeroizing::new(String::new());
    io::stdin().lock().read_line(&mut line)?;
    // Trim trailing newline in-place.
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    Ok(line)
}

async fn restore(seed_phrase: &str) -> Result<()> {
    let config = Config::defaults()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let vault_path = config.data_dir.join("identity.vault");

    if vault_path.exists() {
        anyhow::bail!(
            "identity vault already exists at {}; refusing to overwrite",
            vault_path.display()
        );
    }

    let mnemonic = Mnemonic::parse(seed_phrase);
    let seed = Seed::from_mnemonic(&mnemonic)?;
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

async fn daemon(_detach: bool) -> Result<()> {
    println!("skattr daemon: not yet implemented.");
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
