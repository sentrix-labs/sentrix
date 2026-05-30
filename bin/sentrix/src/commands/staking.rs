//! `sentrix staking …` subcommands — proper TX-based staking ops.
//!
//! Each command crafts a signed `StakingOp` transaction targeted at
//! `PROTOCOL_TREASURY` and injects it into the local mempool. The chain's
//! normal apply path picks it up at the next block, runs the dispatcher
//! at `block_executor::apply_block` (which mutates `stake_registry` AND
//! recomputes `state_trie`), and the change propagates via the standard
//! block-sync route. No direct DB edits, no trie drift.
//!
//! This module exists because `sentrix validator unjail` and
//! `sentrix validator force-unjail` mutate `stake_registry` directly in
//! MDBX `TABLE_STATE` without touching `state_trie` — those commands
//! create a one-way trap requiring a cluster-wide trie reconciliation
//! to recover. The TX path here goes through apply_block so the trie
//! stays consistent on every peer that re-executes the block.
//!
//! Pattern mirrors `commands::token::cli_create_token_tx` — load
//! blockchain, build StakingOp, sign, add to mempool, save. Operator
//! runs against any node's chain.db (typically the validator's own,
//! while it's stopped); on next start the mempool gossips the tx out
//! and an active proposer includes it.
//!
//! Fork notes:
//!   - `RegisterValidator`, `Unjail`, `ClaimRewards`: gated by
//!     `VOYAGER_REWARD_V2_HEIGHT` (activated mainnet h=590100 / testnet
//!     long ago) — usable now.
//!   - `AddSelfStake`: additionally gated by `ADD_SELF_STAKE_HEIGHT`,
//!     which defaults to `u64::MAX` (dispatch dormant). Operator must
//!     set the env var on every validator and halt-all + simul-start
//!     before AddSelfStake will pass apply.

use anyhow::Context;

use sentrix::core::blockchain::Blockchain;
use sentrix::core::transaction::{PROTOCOL_TREASURY, StakingOp, Transaction};
use sentrix::storage::db::Storage;
use sentrix::wallet::keystore::Keystore;
use sentrix::wallet::wallet::Wallet;

use crate::get_db_path;

/// 1 SRX = 100,000,000 sentri (8 decimal places).
const SENTRI_PER_SRX: u64 = 100_000_000;

/// Shared helper: build + sign a StakingOp tx and queue it in mempool.
///
/// `tx_amount` is the value moved from the sender's balance to
/// `PROTOCOL_TREASURY` (in sentri). For:
///   - RegisterValidator / AddSelfStake: must equal the staked amount
///   - Unjail / ClaimRewards: must be 0
fn cli_create_staking_tx(
    bc: &mut Blockchain,
    wallet: &Wallet,
    op: StakingOp,
    tx_amount: u64,
    fee: u64,
) -> anyhow::Result<String> {
    let sk = wallet.get_secret_key()?;
    let pk = wallet.get_public_key()?;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let data = op
        .encode()
        .map_err(|e| anyhow::anyhow!("StakingOp encode failed: {}", e))?;
    let tx = Transaction::new(
        wallet.address.clone(),
        PROTOCOL_TREASURY.to_string(),
        tx_amount,
        fee,
        nonce,
        data,
        bc.chain_id,
        &sk,
        &pk,
    )
    .map_err(|e| anyhow::anyhow!("Transaction::new failed: {}", e))?;
    let txid = tx.txid.clone();
    bc.add_to_mempool(tx)
        .map_err(|e| anyhow::anyhow!("add_to_mempool failed: {}", e))?;
    Ok(txid)
}

/// Load a wallet from a keystore file. Password comes from the
/// `SENTRIX_WALLET_PASSWORD` env var; if unset, prompts on stdin via
/// rpassword so the password never lands in shell history.
fn load_wallet(keystore_path: &str) -> anyhow::Result<Wallet> {
    let ks = Keystore::load(keystore_path)
        .map_err(|e| anyhow::anyhow!("keystore load {}: {}", keystore_path, e))?;
    let password = match std::env::var("SENTRIX_WALLET_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            rpassword::prompt_password("Wallet password: ").context("read password from stdin")?
        }
    };
    let wallet = ks
        .decrypt(&password)
        .map_err(|e| anyhow::anyhow!("keystore decrypt: {}", e))?;
    Ok(wallet)
}

/// `sentrix staking register` — submit a `StakingOp::RegisterValidator`
/// tx so the sender becomes a candidate in the active set after the
/// next epoch boundary.
///
/// `self_stake_srx` is in whole SRX (converted to sentri internally).
/// `commission_rate_bp` is basis points (1000 = 10%, 0–10000 valid).
pub fn cmd_staking_register(
    keystore_path: &str,
    self_stake_srx: u64,
    commission_rate_bp: u16,
    fee: u64,
) -> anyhow::Result<()> {
    if commission_rate_bp > 10_000 {
        anyhow::bail!(
            "commission_rate {} bp is out of range (max 10000 = 100%)",
            commission_rate_bp
        );
    }
    let wallet = load_wallet(keystore_path)?;
    let self_stake = self_stake_srx
        .checked_mul(SENTRI_PER_SRX)
        .ok_or_else(|| anyhow::anyhow!("self_stake overflow"))?;
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let op = StakingOp::RegisterValidator {
        self_stake,
        commission_rate: commission_rate_bp,
        public_key: hex::encode(wallet.get_public_key()?.serialize_uncompressed()),
    };
    let txid = cli_create_staking_tx(&mut bc, &wallet, op, self_stake, fee)?;
    storage.save_blockchain(&bc)?;
    println!("RegisterValidator transaction submitted to mempool!");
    println!("  TxID:        {}", txid);
    println!("  From:        {}", wallet.address);
    println!(
        "  Self-stake:  {} SRX ({} sentri)",
        self_stake_srx, self_stake
    );
    println!(
        "  Commission:  {} bp ({}%)",
        commission_rate_bp,
        commission_rate_bp / 100
    );
    println!("  Status:      pending (will execute when block is mined)");
    println!();
    println!("  NOTE: tx.amount = self_stake is escrowed into PROTOCOL_TREASURY");
    println!("        on apply. Sender wallet must hold >= self_stake + fee.");
    Ok(())
}

/// `sentrix staking add-self-stake` — submit a `StakingOp::AddSelfStake`
/// tx so the sender's self_stake is topped up by `amount_srx`. Common
/// use is unblocking a jailed validator whose self_stake fell below
/// `MIN_SELF_STAKE` after a downtime slash.
///
/// Dispatch is fork-gated by `ADD_SELF_STAKE_HEIGHT` (default
/// `u64::MAX` = disabled). If the env var isn't set on every validator
/// before the block including this tx is mined, the tx will fail apply
/// with a `gated by ADD_SELF_STAKE_HEIGHT fork (currently disabled)`
/// error and the sender's fee is consumed for the failed attempt.
pub fn cmd_staking_add_self_stake(
    keystore_path: &str,
    amount_srx: u64,
    fee: u64,
) -> anyhow::Result<()> {
    let wallet = load_wallet(keystore_path)?;
    let amount = amount_srx
        .checked_mul(SENTRI_PER_SRX)
        .ok_or_else(|| anyhow::anyhow!("amount overflow"))?;
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let op = StakingOp::AddSelfStake { amount };
    let txid = cli_create_staking_tx(&mut bc, &wallet, op, amount, fee)?;
    storage.save_blockchain(&bc)?;
    println!("AddSelfStake transaction submitted to mempool!");
    println!("  TxID:    {}", txid);
    println!("  From:    {}", wallet.address);
    println!("  Amount:  {} SRX ({} sentri)", amount_srx, amount);
    println!("  Status:  pending (will execute when block is mined)");
    println!();
    println!("  NOTE: dispatch requires ADD_SELF_STAKE_HEIGHT env var set on");
    println!("        every active validator. Tx fee is consumed even on");
    println!("        failed apply.");
    Ok(())
}

/// `sentrix staking unjail` — submit a `StakingOp::Unjail` tx so a
/// previously-jailed validator returns to the active set (next epoch
/// boundary). Requires:
///   - `self_stake >= MIN_SELF_STAKE` (use `add-self-stake` first if slashed below)
///   - current block height >= `jail_until` (jail period expired)
///   - validator is not tombstoned (permanent ban — no recovery)
pub fn cmd_staking_unjail(keystore_path: &str, fee: u64) -> anyhow::Result<()> {
    let wallet = load_wallet(keystore_path)?;
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let op = StakingOp::Unjail;
    let txid = cli_create_staking_tx(&mut bc, &wallet, op, 0, fee)?;
    storage.save_blockchain(&bc)?;
    println!("Unjail transaction submitted to mempool!");
    println!("  TxID:    {}", txid);
    println!("  From:    {}", wallet.address);
    println!("  Status:  pending (will execute when block is mined)");
    Ok(())
}

/// `sentrix staking claim-rewards` — submit a `StakingOp::ClaimRewards`
/// tx so the sender's accumulated reward balance (validator-side and
/// delegator-side) transfers from `PROTOCOL_TREASURY` into the sender's
/// account balance.
pub fn cmd_staking_claim_rewards(keystore_path: &str, fee: u64) -> anyhow::Result<()> {
    let wallet = load_wallet(keystore_path)?;
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let op = StakingOp::ClaimRewards;
    let txid = cli_create_staking_tx(&mut bc, &wallet, op, 0, fee)?;
    storage.save_blockchain(&bc)?;
    println!("ClaimRewards transaction submitted to mempool!");
    println!("  TxID:    {}", txid);
    println!("  From:    {}", wallet.address);
    println!("  Status:  pending (will execute when block is mined)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentri_per_srx_constant() {
        // Sanity-pin the unit conversion — protects against accidental
        // decimal-shift typos in the CLI math.
        assert_eq!(SENTRI_PER_SRX, 100_000_000);
        assert_eq!(15_u64 * SENTRI_PER_SRX, 1_500_000_000);
        assert_eq!(15_000_u64 * SENTRI_PER_SRX, 1_500_000_000_000);
    }

    #[test]
    fn test_register_commission_rate_validation_bounds() {
        // Commission > 10000 bp must be rejected by the CLI guard;
        // tx-level dispatch would also reject, but catching at the CLI
        // is friendlier and saves a roundtrip.
        let result = cmd_staking_register("/dev/null", 15_000, 10_001, 10_000);
        let err = result.expect_err("commission > 10000 bp must fail");
        assert!(
            err.to_string().contains("commission_rate"),
            "expected commission-rate error, got {err}"
        );
    }

    #[test]
    fn test_staking_op_encode_roundtrip() {
        // Pin the wire format: encode + decode round-trip cleanly for
        // every variant the CLI emits. Catches any future serde-rename
        // accident that would break already-submitted-but-unmined txs.
        let cases = [
            StakingOp::RegisterValidator {
                self_stake: 1_500_000_000_000,
                commission_rate: 1000,
                public_key: "abcd".into(),
            },
            StakingOp::AddSelfStake {
                amount: 1_500_000_000,
            },
            StakingOp::Unjail,
            StakingOp::ClaimRewards,
        ];
        for op in &cases {
            let encoded = op.encode().expect("encode");
            let decoded = StakingOp::decode(&encoded).expect("decode");
            // PartialEq derived on StakingOp — compare by serialised form
            // to avoid leaning on potentially-absent Eq.
            let re_encoded = decoded.encode().expect("re-encode");
            assert_eq!(encoded, re_encoded, "round-trip mismatch for {op:?}");
        }
    }
}
