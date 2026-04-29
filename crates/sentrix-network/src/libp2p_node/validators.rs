// Two cheap predicates the swarm consults before letting a peer-sourced
// message into the deeper ingest pipeline. Pulled out of the swarm
// event handlers so the same checks can be reused (and unit-tested)
// without dragging the whole libp2p machinery along.
//
// `is_active_bft_signer` is the "is this address allowed to broadcast
// BFT votes / round-status?" gate. Checks the DPoS stake registry
// first (post-Voyager active set), then falls back to the legacy PoA
// authority roster. The validator-finalize path in `bin/sentrix/main.rs`
// has its own copy of this predicate — both sides harden, defence in
// depth.
//
// `block_boundary_reject_reason` is the network-boundary fast-reject.
// Born from the 2026-04-22 3-way state_root fork: one validator's
// chain.db was damaged, it kept producing blocks with state_root=None
// above STATE_ROOT_FORK_HEIGHT, peers were forwarding them into the
// ingest pipeline before the execution-time guard caught and rejected
// them. Putting the cheap checks (chain_id mismatch, missing state_root
// past the fork) at the boundary kills the obviously-bad blocks before
// they contend for the chain write lock — and, importantly, before we
// log a sea of execution-time ERRORs that drown out the actual signal.

use sentrix_primitives::block::{Block, STATE_ROOT_FORK_HEIGHT};

use crate::node::SharedBlockchain;

pub(super) async fn is_active_bft_signer(blockchain: &SharedBlockchain, addr: &str) -> bool {
    let bc = blockchain.read().await;
    if bc.stake_registry.is_active(addr) {
        return true;
    }
    bc.authority.is_active_validator(addr)
}

pub(super) fn block_boundary_reject_reason(
    block: &Block,
    our_chain_id: u64,
) -> Option<&'static str> {
    // H-01: cross-chain block. find the first non-coinbase tx and check its
    // chain_id. (If every tx is coinbase, skip this check — coinbase has
    // no chain_id-bound semantics.)
    if let Some(tx) = block.transactions.iter().find(|t| !t.is_coinbase())
        && tx.chain_id != our_chain_id
    {
        return Some("chain_id mismatch");
    }

    // 2026-04-21 3-way fork guard: past STATE_ROOT_FORK_HEIGHT, every valid
    // block must carry a state_root; missing = producer's trie is broken.
    // The execution-time guard in block_executor.rs also catches this, but
    // gating at the network boundary means we never spend a write lock or
    // apply-task on the bad block — and, more importantly, we don't log it
    // at ERROR from every peer's execution path. One clean WARN at ingest
    // is easier to spot than a flood of mismatches.
    if block.index >= STATE_ROOT_FORK_HEIGHT && block.state_root.is_none() {
        return Some("missing state_root past STATE_ROOT_FORK_HEIGHT (sender's trie is broken)");
    }

    None
}
