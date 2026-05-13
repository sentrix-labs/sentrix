//! `sentrix` miscellaneous subcommands — small standalone CLI handlers
//! that don't share a domain with the other `commands/` modules:
//! `balance`, `history`, `genesis-wallets`.
//!
//! - `balance` / `history` are read-only account queries against the
//!   chain DB.
//! - `genesis-wallets` is a one-shot setup helper: generates seven
//!   labelled keypairs (founder / ecosystem_fund / early_validator /
//!   reserve / genesis_node_1..3) and writes them to
//!   `<wallets_dir>/genesis_wallets.json` for bootstrap-time use.
//!   The file contains private keys in plaintext — the command
//!   prints a CRITICAL reminder to back it up offline and delete
//!   from disk immediately after.

use sentrix::storage::db::Storage;
use sentrix::wallet::wallet::Wallet;

use crate::{get_db_path, get_wallets_dir};

pub fn cmd_balance(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    println!("Address: {}", address);
    println!(
        "Balance: {} sentri ({} SRX)",
        balance,
        balance as f64 / 100_000_000.0
    );
    Ok(())
}

pub fn cmd_genesis_wallets() -> anyhow::Result<()> {
    println!("Generating 7 genesis wallets for Sentrix...\n");

    let roles = [
        "founder",
        "ecosystem_fund",
        "early_validator",
        "reserve",
        "genesis_node_1",
        "genesis_node_2",
        "genesis_node_3",
    ];

    let mut wallets_json = serde_json::json!({});

    for role in &roles {
        let wallet = Wallet::generate();
        println!("[{}]", role.to_uppercase());
        println!("  Address:     {}", wallet.address);
        println!("  Public key:  {}", wallet.public_key);
        println!("  Private key: {}", wallet.secret_key_hex());
        println!();

        wallets_json[*role] = serde_json::json!({
            "address": wallet.address,
            "public_key": wallet.public_key,
            "private_key": wallet.secret_key_hex(),
        });
    }

    // Save to file. On Unix, force mode 0600 (owner read+write only) so
    // a permissive umask can't leak the file to other local users —
    // genesis_wallets.json holds plaintext private keys. CR #652
    // flagged the default-umask path as an information-disclosure risk;
    // CR #654 added: `OpenOptionsExt::mode` only applies to NEW files,
    // so an existing file with broader perms keeps them when truncated.
    // Call `set_permissions` explicitly after open to tighten both paths.
    let output_path = format!("{}/genesis_wallets.json", get_wallets_dir());
    let body = serde_json::to_string_pretty(&wallets_json)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&output_path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&output_path, body)?;
    }
    println!("Saved to: {}", output_path);
    println!("\nCRITICAL: Back up genesis_wallets.json offline immediately.");
    println!("          Delete from this machine after backup.");
    Ok(())
}

pub fn cmd_history(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    let nonce = bc.accounts.get_nonce(address);
    let history = bc.get_address_history(address, 20, 0);

    println!("Address: {}", address);
    println!(
        "Balance: {} sentri ({} SRX)",
        balance,
        balance as f64 / 100_000_000.0
    );
    println!("Nonce:   {}", nonce);
    println!("Transactions: {}\n", history.len());

    for tx in history.iter().rev().take(20) {
        let dir = tx["direction"].as_str().unwrap_or("?");
        let label = match dir {
            "reward" => "REWARD",
            "in" => "IN    ",
            "out" => "OUT   ",
            _ => "?     ",
        };
        // Slice by chars, not bytes, to avoid panicking when the fallback
        // "?" (or any short / non-ascii string) doesn't have 24 bytes —
        // CR #652 flagged this as a 🔴 Critical panic risk. Truncating by
        // char count is the right level of granularity: txids are hex
        // (ASCII), so chars == bytes for the normal path.
        let txid_short: String = tx["txid"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(24)
            .collect();
        println!(
            "  [{}] {} | {} sentri | Block #{}",
            label, txid_short, tx["amount"], tx["block_index"],
        );
    }
    Ok(())
}
