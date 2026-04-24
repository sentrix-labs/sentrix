// keystore-encrypt — encrypt a raw private key (read from STDIN) into a
// password-protected keystore file. Password read from SENTRIX_WALLET_PASSWORD
// env var — never from argv. One-shot tool for cold-storage encryption of
// founder / admin keys that live in Bitwarden as raw hex.

use clap::Parser;
use sentrix_wallet::{Keystore, Wallet};
use std::io::{self, Read};

#[derive(Parser)]
struct Args {
    /// Output keystore file path (e.g. /mnt/adm-key/founder-v3.keystore)
    #[arg(long)]
    output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut privkey_hex = String::new();
    io::stdin().read_to_string(&mut privkey_hex)?;
    let privkey_hex = privkey_hex.trim();
    let privkey_hex = privkey_hex.strip_prefix("0x").unwrap_or(privkey_hex);
    if privkey_hex.len() != 64 {
        return Err(format!(
            "expected 64 hex chars on stdin, got {}",
            privkey_hex.len()
        )
        .into());
    }

    let password = std::env::var("SENTRIX_WALLET_PASSWORD")
        .map_err(|_| "SENTRIX_WALLET_PASSWORD env var required (not passed as argv)")?;
    if password.is_empty() {
        return Err("SENTRIX_WALLET_PASSWORD env var is empty".into());
    }

    let wallet = Wallet::from_private_key(privkey_hex)?;
    let keystore = Keystore::encrypt(&wallet, &password)?;
    keystore.save(&args.output)?;

    // Scrub env var from memory ASAP.
    // SAFETY: just read the variable we want to remove.
    unsafe { std::env::remove_var("SENTRIX_WALLET_PASSWORD") };

    println!("Keystore written:");
    println!("  Address: {}", wallet.address);
    println!("  Path:    {}", args.output);
    println!("  KDF:     argon2id + AES-256-GCM");
    Ok(())
}
