// tests/common/mod.rs — Shared helpers for Sentrix integration tests
#![allow(dead_code)]

use sentrix::core::blockchain::{Blockchain, CHAIN_ID, TOTAL_PREMINE, BLOCK_REWARD};
use sentrix::core::transaction::{Transaction, MIN_TX_FEE};
use sentrix::wallet::wallet::Wallet;

/// Admin address used in all tests (matches FOUNDER_ADDRESS — receives genesis premine).
pub const ADMIN: &str = "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be";

/// A well-formed receiver address used as a destination in various tests.
pub const RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

/// Create a fresh Blockchain with one registered validator.
/// Returns (blockchain, validator_wallet). The validator is added via the real
/// add_validator() path (crypto-validated) — no unchecked shortcuts.
pub fn setup_single_validator() -> (Blockchain, Wallet) {
    let mut bc = Blockchain::new(ADMIN.to_string());
    let val = Wallet::generate();
    bc.authority
        .add_validator(ADMIN, val.address.clone(), "Test Validator".to_string(), val.public_key.clone())
        .expect("add_validator failed");
    (bc, val)
}

/// Mine one empty block (coinbase only, no user transactions).
pub fn mine_empty_block(bc: &mut Blockchain, validator_addr: &str) {
    let block = bc.create_block(validator_addr).expect("create_block failed");
    bc.add_block(block).expect("add_block failed");
}

/// Mine a block that includes whatever is currently in the mempool.
pub fn mine_block_with_mempool(bc: &mut Blockchain, validator_addr: &str) {
    mine_empty_block(bc, validator_addr);
}

/// Create a signed SRX transfer transaction. Nonce is read from the chain's
/// confirmed nonce for `wallet.address` — use this for the first TX only.
pub fn make_tx(bc: &Blockchain, wallet: &Wallet, to: &str, amount: u64, fee: u64) -> Transaction {
    let nonce = bc.accounts.get_nonce(&wallet.address);
    make_tx_nonce(wallet, to, amount, fee, nonce)
}

/// Create a signed SRX transfer transaction with an explicit nonce.
/// Use this when submitting multiple pending TXs from the same sender.
pub fn make_tx_nonce(wallet: &Wallet, to: &str, amount: u64, fee: u64, nonce: u64) -> Transaction {
    let sk = wallet.get_secret_key().expect("get_secret_key failed");
    let pk = wallet.get_public_key().expect("get_public_key failed");
    Transaction::new(
        wallet.address.clone(),
        to.to_string(),
        amount,
        fee,
        nonce,
        String::new(),
        CHAIN_ID,
        &sk,
        &pk,
    )
    .expect("Transaction::new failed")
}

/// Expected total_minted (in sentri) for a given block height.
/// Valid for heights << HALVING_INTERVAL (42M blocks — never reached in tests).
pub fn expected_total_minted(height: u64) -> u64 {
    TOTAL_PREMINE + height * BLOCK_REWARD
}

/// Fund a fresh wallet with `amount` sentri and return the wallet.
pub fn funded_wallet(bc: &mut Blockchain, amount: u64) -> Wallet {
    let w = Wallet::generate();
    bc.accounts.credit(&w.address, amount).expect("credit failed");
    w
}
