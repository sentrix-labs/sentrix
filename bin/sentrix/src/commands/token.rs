//! `sentrix token …` subcommands — Sentrix-native token-op
//! (TOKEN_OP_ADDRESS) deploy / transfer / burn / balance / info / list.
//!
//! Extracted from `main.rs`. Same pattern as the other `commands/`
//! modules: pure CLI handlers, real work lives in
//! `sentrix-core::token_ops` / the TokenOp encoding in
//! `sentrix-core::transaction`.
//!
//! `cli_create_token_tx` is the shared helper for the three mutating
//! commands (deploy / transfer / burn). It assembles a TokenOp tx,
//! signs with the caller's wallet, and pushes into the mempool — no
//! state mutation beyond mempool admission. read-only cmds (balance /
//! info / list) hit the chain DB directly via Blockchain accessors.

use sentrix::core::blockchain::Blockchain;
use sentrix::core::transaction::{TOKEN_OP_ADDRESS, TokenOp, Transaction};
use sentrix::storage::db::Storage;
use sentrix::wallet::wallet::Wallet;

use crate::get_db_path;

fn cli_create_token_tx(
    bc: &mut Blockchain,
    wallet: &Wallet,
    token_op: TokenOp,
    fee: u64,
) -> anyhow::Result<String> {
    let sk = wallet.get_secret_key()?;
    let pk = wallet.get_public_key()?;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let data = token_op.encode()?;
    let tx = Transaction::new(
        wallet.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0,
        fee,
        nonce,
        data,
        bc.chain_id,
        &sk,
        &pk,
    )?;
    let txid = tx.txid.clone();
    bc.add_to_mempool(tx)?;
    Ok(txid)
}

pub fn cmd_token_deploy(
    name: &str,
    symbol: &str,
    decimals: u8,
    supply: u64,
    deployer_key: &str,
    fee: u64,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(deployer_key)?;
    let token_op = TokenOp::Deploy {
        name: name.to_string(),
        symbol: symbol.to_string(),
        decimals,
        supply,
        max_supply: 0,
    };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, fee)?;
    storage.save_blockchain(&bc)?;
    println!("Token deploy transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  Name:     {}", name);
    println!("  Symbol:   {}", symbol);
    println!("  Supply:   {}", supply);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

pub fn cmd_token_transfer(
    contract: &str,
    to: &str,
    amount: u64,
    from_key: &str,
    gas: u64,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Transfer {
        contract: contract.to_string(),
        to: to.to_string(),
        amount,
    };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, gas)?;
    storage.save_blockchain(&bc)?;
    println!("Token transfer transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  From:     {}", wallet.address);
    println!("  To:       {}", to);
    println!("  Amount:   {}", amount);
    println!("  Contract: {}", contract);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

pub fn cmd_token_burn(
    contract: &str,
    amount: u64,
    from_key: &str,
    gas: u64,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Burn {
        contract: contract.to_string(),
        amount,
    };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, gas)?;
    storage.save_blockchain(&bc)?;
    println!("Token burn transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  From:     {}", wallet.address);
    println!("  Amount:   {} burned", amount);
    println!("  Contract: {}", contract);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

pub fn cmd_token_balance(contract: &str, address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.token_balance(contract, address);
    println!("Token balance:");
    println!("  Address:  {}", address);
    println!("  Contract: {}", contract);
    println!("  Balance:  {}", balance);
    Ok(())
}

pub fn cmd_token_info(contract: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let info = bc.token_info(contract)?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

pub fn cmd_token_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let tokens = bc.list_tokens();
    if tokens.is_empty() {
        println!("No tokens deployed yet.");
        return Ok(());
    }
    println!("Deployed tokens ({}):", tokens.len());
    for token in &tokens {
        println!(
            "  [{}] {} ({}) — supply: {}",
            token["contract_address"].as_str().unwrap_or(""),
            token["name"].as_str().unwrap_or(""),
            token["symbol"].as_str().unwrap_or(""),
            token["total_supply"],
        );
    }
    Ok(())
}
