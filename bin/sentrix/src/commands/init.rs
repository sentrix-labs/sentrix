//! `sentrix init` — one-shot chain bootstrap. Creates the genesis block
//! from either an embedded mainnet template or a custom TOML at
//! `--genesis <path>` (for testnet / devnet / one-off chains), persists
//! it as height 0, and prints the post-init state for the operator.
//!
//! Extracted from `main.rs`. Same pattern as the other `commands/`
//! modules — pure CLI handler, no consensus path touched. The actual
//! genesis assembly lives in `sentrix-core::Genesis` /
//! `Blockchain::new_with_genesis`.

use sentrix::core::blockchain::Blockchain;
use sentrix::storage::db::Storage;

use crate::get_db_path;

pub fn cmd_init(admin: &str, genesis_path: Option<&str>) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if storage.has_blockchain() {
        println!("Chain already initialized.");
        return Ok(());
    }
    // Load + validate genesis config up front so a malformed config aborts
    // init before we touch storage. A custom --genesis path lets operators
    // bootstrap non-mainnet chains (testnet, devnet) from TOML without
    // rebuilding the binary.
    let genesis = match genesis_path {
        Some(path) => {
            let g = sentrix::core::Genesis::from_path(path)?;
            println!(
                "Loaded genesis from {}: chain_id={} ({})",
                path, g.chain.chain_id, g.chain.name
            );
            g
        }
        None => {
            let g = sentrix::core::Genesis::mainnet()?;
            println!(
                "Using embedded mainnet genesis: chain_id={} ({})",
                g.chain.chain_id, g.chain.name
            );
            g
        }
    };
    let mut bc = Blockchain::new_with_genesis(admin.to_string(), &genesis);
    // Seat the genesis validators into the authority set so a fresh chain has
    // a set to produce from and to migrate into the stake registry at the
    // Voyager fork. Done here (node init) rather than in the constructor so
    // the bare Blockchain::new stays a clean slate.
    bc.seat_genesis_validators(&genesis);
    storage.save_blockchain(&bc)?;
    let premine_srx = genesis.total_premine() / 100_000_000;
    println!("Chain initialized.");
    println!("Admin address: {}", admin);
    println!("Genesis block created. Height: 0");
    println!("Total premine: {} SRX", premine_srx);
    Ok(())
}
