//! `sentrix validator …` subcommands — admin-only operations on the
//! validator set (add / remove / toggle / rename / unjail / transfer-
//! admin). Every command opens the chain DB, mutates the authority
//! manager or stake registry, and saves back.
//!
//! Extracted from `main.rs`. Same pattern as
//! [`crate::commands::wallet`]: pure CLI handlers, no consensus path
//! touched — the underlying mutations live in `sentrix-core::authority`
//! and `sentrix-staking`.

use sentrix::storage::db::Storage;
use sentrix::wallet::wallet::Wallet;

use crate::get_db_path;

pub fn cmd_validator_add(
    address: &str,
    name: &str,
    public_key: &str,
    admin_key: &str,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority.add_validator(
        &admin_wallet.address,
        address.to_string(),
        name.to_string(),
        public_key.to_string(),
    )?;

    storage.save_blockchain(&bc)?;
    println!("Validator added: {} ({})", name, address);
    Ok(())
}

pub fn cmd_validator_unjail(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let height = bc.height();
    bc.stake_registry.unjail(address, height)?;
    bc.stake_registry.update_active_set();

    storage.save_blockchain(&bc)?;
    println!("Validator unjailed: {}", address);
    println!(
        "Active set: {} validators",
        bc.stake_registry.active_count()
    );
    Ok(())
}

pub fn cmd_validator_force_unjail(
    address: &str,
    acknowledged_phantom_stake: bool,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    const MAINNET_CHAIN_ID: u64 = 7119;
    if bc.chain_id == MAINNET_CHAIN_ID && !acknowledged_phantom_stake {
        anyhow::bail!(
            "mainnet (chain_id 7119) detected: force-unjail creates phantom \
             stake (restores self_stake without minting SRX), which violates \
             the supply invariant. Prefer a real self-delegate TX from the \
             validator's wallet. If the chain is genuinely stuck and this is \
             the last option, re-run with `--i-understand-phantom-stake`."
        );
    }
    if bc.chain_id == MAINNET_CHAIN_ID {
        eprintln!(
            "WARNING: force-unjail on mainnet. Phantom stake will be created \
             if self_stake < MIN_SELF_STAKE. Document the recovery decision \
             before proceeding."
        );
    }

    let before = bc
        .stake_registry
        .get_validator(address)
        .map(|v| (v.self_stake, v.is_jailed, v.jail_until));
    bc.stake_registry.force_unjail(address)?;
    bc.stake_registry.update_active_set();
    let after = bc
        .stake_registry
        .get_validator(address)
        .map(|v| (v.self_stake, v.is_jailed, v.jail_until));

    storage.save_blockchain(&bc)?;
    println!("Validator force-unjailed: {}", address);
    if let (Some(b), Some(a)) = (before, after) {
        println!("  self_stake: {} → {}", b.0, a.0,);
        println!("  is_jailed:  {} → {}", b.1, a.1,);
        println!("  jail_until: {} → {}", b.2, a.2,);
    }
    println!(
        "Active set: {} validators",
        bc.stake_registry.active_count()
    );

    // The mutation above writes stake_registry to TABLE_STATE but does
    // NOT update the state_trie. After 2026-04-25's verify-deep gate,
    // chain.db that's been touched by force-unjail without a trie
    // rebuild will fail the boot-time consistency check on subsequent
    // peers — discovered live during the 2026-04-27 unjail attempt.
    // The recovery is a cluster-wide trie rebuild from the post-edit
    // AccountDB. Print the canonical procedure here so the operator
    // doesn't have to remember it from the runbook.
    println!();
    println!("NEXT STEPS — cluster-wide trie reconciliation:");
    println!();
    println!("  This command edited stake_registry but did NOT update the");
    println!("  state_trie. Other peers will reject the post-edit chain.db");
    println!("  via verify-deep until you complete the cluster-wide rebuild.");
    println!();
    println!("  1. Confirm ALL other peers are halted (systemctl is-active).");
    println!("  2. On THIS peer, drop the trie tables:");
    println!("       sentrix chain reset-trie --i-understand-divergence-risk");
    println!("  3. tar-pipe THIS peer's chain.db to every other peer:");
    println!("       ssh canonical 'tar -C <data_dir> -cf - chain.db' \\");
    println!("         | ssh peer 'tar -C <data_dir> -xf - --overwrite'");
    println!("  4. Simultaneous-start all peers. Each peer's init_trie will");
    println!("     backfill the trie from the (identical) AccountDB → all");
    println!("     peers converge on the same backfill-shape trie.");
    println!();
    println!("  Until step 4, the chain remains halted. Skipping the");
    println!("  reset-trie step on this peer or any other peer will fork.");

    Ok(())
}

pub fn cmd_validator_transfer_admin(new_admin: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let admin_wallet = Wallet::from_private_key(admin_key)?;
    let old_admin = bc.authority.admin_address.clone();

    bc.authority
        .transfer_admin(&admin_wallet.address, new_admin.to_string())?;

    storage.save_blockchain(&bc)?;
    println!("Admin role transferred:");
    println!("  old: {}", old_admin);
    println!("  new: {}", new_admin);
    println!("Note: this only updates THIS node's chain DB. Run on every");
    println!("validator's DB to complete cluster-wide rotation.");
    Ok(())
}

pub fn cmd_validator_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    println!(
        "Validators ({} total, {} active):",
        bc.authority.validator_count(),
        bc.authority.active_count()
    );
    for v in bc.authority.active_validators() {
        println!(
            "  [{}] {} — {} blocks produced",
            if v.is_active { "ACTIVE" } else { "INACTIVE" },
            v.name,
            v.blocks_produced
        );
        println!("      Address: {}", v.address);
    }
    Ok(())
}

pub fn cmd_validator_remove(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority
        .remove_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    println!("Validator removed: {}", address);
    Ok(())
}

pub fn cmd_validator_toggle(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    let is_active = bc
        .authority
        .toggle_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    let status = if is_active { "ACTIVE" } else { "INACTIVE" };
    println!("Validator {} toggled to: {}", address, status);
    Ok(())
}

pub fn cmd_validator_rename(address: &str, new_name: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority
        .rename_validator(&admin_wallet.address, address, new_name.to_string())?;
    storage.save_blockchain(&bc)?;
    println!("Validator {} renamed to: {}", address, new_name);
    Ok(())
}
