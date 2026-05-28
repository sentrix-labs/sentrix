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
use zeroize::Zeroizing;

use crate::get_wallets_dir;

pub fn cmd_wallet_generate(password: Option<String>) -> anyhow::Result<()> {
    let wallet = Wallet::generate();
    println!("\nNew wallet generated:");
    println!("  Address:     {}", wallet.address);
    println!("  Public key:  {}", wallet.public_key);

    if let Some(pwd) = password {
        let pwd = reject_empty_cli_password(pwd)?;
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
        let pwd = reject_empty_cli_password(pwd)?;
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
    // New password: same resolution path, plus confirm-twice when the
    // value comes from an interactive prompt. CLI / env paths still
    // take the single value (no second source to confirm against);
    // we trust the operator's automation when they explicitly pass
    // the password non-interactively.
    let new_pwd = resolve_password_named_confirmed(
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
    // Two-step rename: backup the original, then install the new
    // keystore. If the install fails, restore the backup so the
    // canonical keystore path is never missing — otherwise the
    // operator has to manually move `.bak-*` back before the
    // validator can boot. The restore's own error is intentionally
    // swallowed; we surface the original install failure since that
    // is the actionable cause.
    std::fs::rename(path, &bak_path)?;
    if let Err(install_err) = std::fs::rename(&tmp_path, path) {
        let restored = std::fs::rename(&bak_path, path).is_ok();
        let note = if restored {
            "original keystore restored from backup"
        } else {
            "original keystore could NOT be restored — check .bak file manually"
        };
        anyhow::bail!(
            "failed to install rekeyed keystore: {} ({})",
            install_err,
            note
        );
    }

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

/// Reject `--password ""` (and `"   "`, `"\n"`, …) from any subcommand
/// that takes a CLI password directly (generate, import). All three
/// entry points (CLI helper, env helper, prompt helper) normalise the
/// same way — trim then reject empty — so the same operator input
/// behaves identically regardless of source. Returns the trimmed
/// password wrapped in `Zeroizing` so the heap allocation is wiped on
/// drop (matching how the rest of the binary handles secret material —
/// `Wallet::secret_key_bytes`, the `SENTRIX_VALIDATOR_KEY` env var).
///
/// Takes the password by value (not `&str`) so the caller's untrimmed
/// `String` allocation is moved into a `Zeroizing` envelope and wiped
/// on drop too. A previous version took `&str` and the caller's heap
/// buffer (from clap / env) survived unzeroed; CodeRabbit caught it on
/// PR #646.
fn reject_empty_cli_password(pw: String) -> anyhow::Result<Zeroizing<String>> {
    // Wrap the caller's allocation first so it can't leak if the
    // trim-clone below panics or bails early.
    let pw = Zeroizing::new(pw);
    let trimmed = pw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--password cannot be empty or whitespace");
    }
    Ok(Zeroizing::new(trimmed.to_string()))
}

/// Hidden terminal prompt — characters are not echoed. Replaces the old
/// `stdin().read_line()` path which printed the password to the screen
/// (visible to anyone shoulder-surfing, and persisted in scrollback
/// buffers / tmux capture / asciinema recordings).
fn read_password_hidden(prompt: &str) -> anyhow::Result<Zeroizing<String>> {
    let raw = rpassword::prompt_password(format!("{}: ", prompt))?;
    // Wrap immediately so the underlying allocation is wiped even if we
    // bail out below; the trim/clone produces a fresh allocation we
    // also keep inside `Zeroizing`.
    let raw = Zeroizing::new(raw);
    let pw = Zeroizing::new(raw.trim().to_string());
    if pw.is_empty() {
        anyhow::bail!("Password cannot be empty or whitespace");
    }
    Ok(pw)
}

/// Validate a CLI / env password value. Trims (matching the prompt
/// path's behaviour) and rejects strictly empty + whitespace-only
/// values from every source. Returns the trimmed password wrapped in
/// `Zeroizing` so all three helpers produce the same canonical form
/// for `Keystore::encrypt` — and the heap buffer is wiped on drop.
fn validate_external_password(pw: String, source: &str) -> anyhow::Result<Zeroizing<String>> {
    // Wrap the raw source allocation BEFORE the trim/clone so it can't
    // leak via reallocation if we bail.
    let pw = Zeroizing::new(pw);
    let pw = Zeroizing::new(pw.trim().to_string());
    if pw.is_empty() {
        anyhow::bail!("Password from {} cannot be empty or whitespace", source);
    }
    Ok(pw)
}

/// Like [`resolve_password`] but with a named env var + custom prompt.
/// Lets `rekey` distinguish OLD vs NEW password sources cleanly.
pub fn resolve_password_named(
    cli_password: Option<String>,
    env_var: &str,
    prompt: &str,
) -> anyhow::Result<Zeroizing<String>> {
    if let Some(pw) = cli_password {
        return validate_external_password(pw, "--password");
    }
    if let Ok(pw) = std::env::var(env_var) {
        return validate_external_password(pw, env_var);
    }
    read_password_hidden(prompt)
}

/// Like [`resolve_password_named`] but confirms the value by prompting
/// twice when it comes from an interactive prompt. Used for the NEW
/// password on `wallet rekey` — a typo would otherwise re-encrypt the
/// keystore to an unintended password (the self-decrypt check still
/// passes because both encrypt and verify use the same mistyped value).
///
/// CLI / env paths are accepted as-is. They're non-interactive sources;
/// there's no second value to compare against, and operators using
/// automation have already committed to whatever they passed in.
pub fn resolve_password_named_confirmed(
    cli_password: Option<String>,
    env_var: &str,
    prompt: &str,
) -> anyhow::Result<Zeroizing<String>> {
    if let Some(pw) = cli_password {
        return validate_external_password(pw, "--password");
    }
    if let Ok(pw) = std::env::var(env_var) {
        return validate_external_password(pw, env_var);
    }
    let first = read_password_hidden(prompt)?;
    let second = read_password_hidden(&format!("{} (confirm)", prompt))?;
    if *first != *second {
        anyhow::bail!("Passwords did not match — aborting");
    }
    Ok(first)
}

/// Resolve password from CLI arg, `SENTRIX_WALLET_PASSWORD` env var, or
/// terminal prompt. Called from the wallet subcommands here and from
/// `main.rs::cmd_start` (validator boot keystore decrypt). Returns
/// `Zeroizing<String>` so the heap buffer is wiped on drop — passwords
/// MUST NOT linger in process memory or crash dumps.
pub fn resolve_password(cli_password: Option<String>) -> anyhow::Result<Zeroizing<String>> {
    if let Some(pw) = cli_password {
        return validate_external_password(pw, "--password");
    }
    if let Ok(pw) = std::env::var("SENTRIX_WALLET_PASSWORD") {
        return validate_external_password(pw, "SENTRIX_WALLET_PASSWORD");
    }
    read_password_hidden("Enter wallet password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_empty_cli_password_rejects_empty() {
        assert!(reject_empty_cli_password(String::new()).is_err());
    }

    #[test]
    fn reject_empty_cli_password_rejects_whitespace_only() {
        // Operators sometimes paste a trailing space or newline into
        // `--password "..."`; the prompt path trims so the CLI / env
        // paths must too, otherwise the same operator input produces
        // a different keystore depending on which entry point ran.
        assert!(reject_empty_cli_password("   ".into()).is_err());
        assert!(reject_empty_cli_password("\n".into()).is_err());
        assert!(reject_empty_cli_password("\t\t".into()).is_err());
    }

    #[test]
    fn reject_empty_cli_password_accepts_non_empty() {
        let pw = reject_empty_cli_password("hunter2".into()).unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn reject_empty_cli_password_returns_trimmed() {
        // Caller uses the returned value, not the original arg — so a
        // password typed as " hunter2 " encrypts the keystore with
        // exactly "hunter2", same as the prompt path.
        let pw = reject_empty_cli_password("  hunter2  ".into()).unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn validate_external_password_rejects_empty() {
        let err = validate_external_password(String::new(), "--password").unwrap_err();
        assert!(err.to_string().contains("--password"));
    }

    #[test]
    fn validate_external_password_rejects_whitespace_only() {
        let err = validate_external_password("   ".into(), "SENTRIX_WALLET_PASSWORD").unwrap_err();
        assert!(err.to_string().contains("SENTRIX_WALLET_PASSWORD"));
        assert!(err.to_string().contains("whitespace"));
    }

    #[test]
    fn validate_external_password_returns_trimmed() {
        let pw = validate_external_password("  hunter2  ".into(), "--password").unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn validate_external_password_passes_non_empty() {
        let pw = validate_external_password("hunter2".into(), "--password").unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn resolve_password_named_cli_empty_string_rejected() {
        // `--password ""` used to slip through the cli_password branch and
        // encrypt the keystore with an empty string, silently bypassing the
        // prompt path's own empty-check. Pin the rejection here.
        let err = resolve_password_named(
            Some(String::new()),
            "SENTRIX_TEST_NEVER_SET_PWD",
            "Enter test password",
        )
        .unwrap_err();
        assert!(err.to_string().contains("--password"));
    }

    #[test]
    fn resolve_password_named_cli_non_empty_returned_verbatim() {
        let pw = resolve_password_named(
            Some("hunter2".into()),
            "SENTRIX_TEST_NEVER_SET_PWD",
            "Enter test password",
        )
        .unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn resolve_password_cli_empty_string_rejected() {
        let err = resolve_password(Some(String::new())).unwrap_err();
        assert!(err.to_string().contains("--password"));
    }

    #[test]
    fn resolve_password_named_confirmed_cli_path_skips_confirmation() {
        // CLI / env are non-interactive sources — confirmation has no
        // second value to check against, so we accept them as-is (only
        // the empty-string guard applies).
        let pw = resolve_password_named_confirmed(
            Some("hunter2".into()),
            "SENTRIX_TEST_NEVER_SET_PWD",
            "Enter NEW wallet password",
        )
        .unwrap();
        assert_eq!(pw.as_str(), "hunter2");
    }

    #[test]
    fn resolve_password_named_confirmed_cli_empty_rejected() {
        let err = resolve_password_named_confirmed(
            Some(String::new()),
            "SENTRIX_TEST_NEVER_SET_PWD",
            "Enter NEW wallet password",
        )
        .unwrap_err();
        assert!(err.to_string().contains("--password"));
    }

    #[test]
    fn resolved_passwords_are_zeroizing_wrapped() {
        // Type-level proof — the return type *is* Zeroizing<String>, so a
        // dropped password gets its heap buffer wiped (no recovery from
        // process memory / crash dumps). If this compiles, the wrap is
        // intact; if a future refactor drops the wrap by accident the
        // assignment fails at build time.
        let _proof: Zeroizing<String> = resolve_password_named(Some("x".into()), "_", "_").unwrap();
        let _proof: Zeroizing<String> =
            resolve_password_named_confirmed(Some("x".into()), "_", "_").unwrap();
        let _proof: Zeroizing<String> = resolve_password(Some("x".into())).unwrap();
        let _proof: Zeroizing<String> = reject_empty_cli_password("x".into()).unwrap();
        let _proof: Zeroizing<String> =
            validate_external_password("x".into(), "--password").unwrap();
    }
}
