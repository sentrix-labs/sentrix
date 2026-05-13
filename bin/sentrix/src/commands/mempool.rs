//! `sentrix mempool …` subcommands — admin operations on the pending
//! transaction pool (clear, stats).
//!
//! Extracted from `main.rs`. Same pattern as the other `commands/`
//! modules: pure CLI handlers, the mempool itself lives in
//! `sentrix-core::mempool` and is reached through `Blockchain::mempool_size`
//! / `clear_mempool`.
//!
//! `mempool clear` is destructive (drops every pending tx). It's a
//! local-only operation — peers still hold their own copies and will
//! re-broadcast on the next tick — but the operator's own
//! recently-submitted tx will need re-sending if it wasn't already
//! gossiped out.

use sentrix::storage::db::Storage;

use crate::get_db_path;

pub fn cmd_mempool_clear() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let old_size = bc.mempool_size();
    bc.clear_mempool();
    storage.save_blockchain(&bc)?;
    println!("Mempool cleared: {} transactions removed", old_size);
    Ok(())
}

pub fn cmd_mempool_stats() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    println!("Mempool size: {} transactions", bc.mempool_size());
    Ok(())
}
