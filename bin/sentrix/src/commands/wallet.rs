//! `sentrix wallet …` subcommands — keystore + private-key operations.
//!
//! Extracted from `main.rs` so all wallet/keystore CLI surface lives in
//! one place. None of these functions touch the blockchain — pure
//! cryptographic / filesystem operations.
//!
//! Password resolution helpers ([`resolve_password`],
//! [`resolve_password_named`]) live here too because the validator
//! boot path in `main.rs::cmd_start` also calls them — keeping all
//! password-handling code in one module avoids a stray copy elsewhere
//! drifting on prompt UX or env-var name.

use sentrix::wallet::keystore::Keystore;
use sentrix::wallet::wallet::Wallet;

use crate::get_wallets_dir;

pub fn cmd_wallet_generate(password: Option<String>) -> anyhow::Result<()> {
    let wallet = Wallet::generate();
    println!("\nNew wallet generated:");
    println!("  Address:     {}", wallet.address);
    println!("  Public key:  {}", wallet.public_key);

    if let Some(pwd) = password {
        let keystore = Keystore::encrypt(&wallet, &pwd)?;
        let filename = format!("{}/{}.json", get_wallets_dir(), &wallet.address[2..10]);
        keystore.save(&filename)?;
        println!("  Keystore:    {}", filename);
        println!("\nWARNING: Back up your keystore file and password securely.");
    } else {
        println!("  Private key: {}", wallet.secret_key_hex());
        println!("\nWARNING: Save your private key securely. It will not be shown again.");
    }
    Ok(())
}

pub fn cmd_wallet_import(private_key: &str, password: Option<String>) -> anyhow::Result<()> {
    let wallet = Wallet::from_private_key(private_key)?;
    println!("Wallet imported:");
    println!("  Address:    {}", wallet.address);
    println!("  Public key: {}", wallet.public_key);

    if let Some(pwd) = password {
        let keystore = Keystore::encrypt(&wallet, &pwd)?;
        let filename = format!("{}/{}.json", get_wallets_dir(), &wallet.address[2..10]);
        keystore.save(&filename)?;
        println!("  Saved to:   {}", filename);
    }
    Ok(())
}

pub fn cmd_wallet_info(keystore_file: &str) -> anyhow::Result<()> {
    let keystore = Keystore::load(keystore_file)?;
    println!("Keystore info:");
    println!("  Address: {}", keystore.address);
    println!("  Cipher:  {}", keystore.crypto.cipher);
    println!(
        "  KDF:     {} ({} iterations)",
        keystore.crypto.kdf, keystore.crypto.kdf_iterations
    );
    Ok(())
}

pub fn cmd_wallet_encrypt(
    private_key: &str,
    password: Option<String>,
    output: Option<String>,
) -> anyhow::Result<()> {
    let pwd = resolve_password(password)?;
    let wallet = Wallet::from_private_key(private_key)?;
    let keystore = Keystore::encrypt(&wallet, &pwd)?;
    let filename = output.unwrap_or_else(|| {
        let dir = get_wallets_dir();
        let _ = std::fs::create_dir_all(&dir);
        format!("{}/{}.json", dir, &wallet.address[2..10])
    });
    keystore.save(&filename)?;
    println!("Wallet encrypted:");
    println!("  Address:  {}", wallet.address);
    println!("  Saved to: {}", filename);
    println!("  KDF:      argon2id");
    Ok(())
}

pub fn cmd_wallet_decrypt(keystore_file: &str, password: Option<String>) -> anyhow::Result<()> {
    let pwd = resolve_password(password)?;
    let keystore = Keystore::load(keystore_file)?;
    let wallet = keystore.decrypt(&pwd)?;
    println!("Wallet decrypted:");
    println!("  Address:     {}", wallet.address);
    println!("  Public key:  {}", wallet.public_key);
    // Private key printed to stdout ONLY — never logged, never in API
    println!("  Private key: {}", wallet.secret_key_hex());
    Ok(())
}

/// Rotate a keystore's password without exposing the private key to
/// disk or logs. Atomic: decrypt → re-encrypt → verify round-trip →
/// backup old file → rename new file over old.
///
/// The private key lives only inside the in-memory `Wallet` struct,
/// which zeroises its secret on drop (`Zeroizing<[u8;32]>`). No
/// stdout/stderr output reveals the key. Only the ADDRESS is printed
/// for operator confirmation.
pub fn cmd_wallet_rekey(
    keystore_file: &str,
    old_password: Option<String>,
    new_password: Option<String>,
) -> anyhow::Result<()> {
    use std::path::Path;

    // Resolve old password via CLI / SENTRIX_WALLET_OLD_PASSWORD env /
    // prompt. Prompt happens if both unset.
    let old_pwd = resolve_password_named(
        old_password,
        "SENTRIX_WALLET_OLD_PASSWORD",
        "Enter OLD wallet password",
    )?;
    // New password: same resolution path + confirm-twice on prompt.
    let new_pwd = resolve_password_named(
        new_password,
        "SENTRIX_WALLET_NEW_PASSWORD",
        "Enter NEW wallet password",
    )?;
    if old_pwd == new_pwd {
        anyhow::bail!("new password is identical to old — rotation would be a no-op");
    }

    // Step 1 — decrypt old keystore (this also validates old_pwd).
    let old_keystore = Keystore::load(keystore_file)?;
    let wallet = old_keystore
        .decrypt(&old_pwd)
        .map_err(|e| anyhow::anyhow!("old password rejected: {}", e))?;
    let address = wallet.address.clone();

    // Step 2 — re-encrypt with new_pwd (fresh salt, nonce, mac).
    let new_keystore = Keystore::encrypt(&wallet, &new_pwd)?;

    // Step 3 — verify round-trip BEFORE touching the original file.
    // If any implementation bug produces an un-decryptable keystore,
    // we catch it here instead of after overwriting the operator's
    // only copy.
    let verify = new_keystore
        .decrypt(&new_pwd)
        .map_err(|e| anyhow::anyhow!("new keystore failed self-decrypt — aborting: {}", e))?;
    if verify.address != address {
        anyhow::bail!(
            "address mismatch after rekey self-verify (got {}, expected {}); aborting",
            verify.address,
            address
        );
    }

    // Step 4 — atomic replace via sibling tempfile + rename.
    let path = Path::new(keystore_file);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("keystore_file has no parent directory"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tmp_path = parent.join(format!(".rekey-tmp-{}", ts));
    new_keystore.save(tmp_path.to_str().ok_or_else(|| {
        anyhow::anyhow!("tempfile path contains non-UTF-8 bytes — refusing to save")
    })?)?;

    // Timestamped backup of the old file. Operator can `rm` after a
    // stable period (suggested 48 h).
    let bak_path = parent.join(format!(
        "{}.bak-{}",
        path.file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("keystore"),
        ts
    ));
    std::fs::rename(path, &bak_path)?;
    std::fs::rename(&tmp_path, path)?;

    // Drop the in-memory plaintext as early as possible. `Wallet`
    // already zeroises its secret on drop, but explicit drop pins
    // the timing.
    drop(old_pwd);
    drop(new_pwd);
    drop(wallet);
    drop(verify);

    println!("Keystore rekeyed:");
    println!("  Address:   {}", address);
    println!("  File:      {}", keystore_file);
    println!("  Old copy:  {}", bak_path.display());
    println!();
    println!("Next steps (operator):");
    println!("  1. Update SENTRIX_WALLET_PASSWORD in the env file to the new password.");
    println!("  2. Restart the validator service (e.g. `systemctl restart sentrix-node`).");
    println!(
        "  3. Confirm 'Validator mode: {}' appears in journalctl.",
        address
    );
    println!(
        "  4. After the node runs stable for 48h, delete {}.",
        bak_path.display()
    );
    Ok(())
}

/// Like [`resolve_password`] but with a named env var + custom prompt.
/// Lets `rekey` distinguish OLD vs NEW password sources cleanly.
pub fn resolve_password_named(
    cli_password: Option<String>,
    env_var: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    if let Some(pw) = cli_password {
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var(env_var) {
        return Ok(pw);
    }
    eprint!("{}: ", prompt);
    let mut pw = String::new();
    std::io::stdin().read_line(&mut pw)?;
    let pw = pw.trim().to_string();
    if pw.is_empty() {
        anyhow::bail!("Password cannot be empty");
    }
    Ok(pw)
}

/// Resolve password from CLI arg, `SENTRIX_WALLET_PASSWORD` env var, or
/// terminal prompt. Called from the wallet subcommands here and from
/// `main.rs::cmd_start` (validator boot keystore decrypt).
pub fn resolve_password(cli_password: Option<String>) -> anyhow::Result<String> {
    if let Some(pw) = cli_password {
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var("SENTRIX_WALLET_PASSWORD") {
        return Ok(pw);
    }
    // Prompt on terminal
    eprint!("Enter wallet password: ");
    let mut pw = String::new();
    std::io::stdin().read_line(&mut pw)?;
    let pw = pw.trim().to_string();
    if pw.is_empty() {
        anyhow::bail!("Password cannot be empty");
    }
    Ok(pw)
}
