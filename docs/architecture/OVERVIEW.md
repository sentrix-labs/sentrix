# Architecture Overview

Sentrix is a Layer-1 blockchain written in Rust. PoA consensus, account-based model (like Ethereum), custom Binary Sparse Merkle Tree for state proofs.

## Components

```
┌──────────────────────────────────────┐
│              CLI (clap)              │
├─────────┬─────────┬────────┬────────┤
│  Core   │ Network │  API   │ Wallet │
│         │         │        │        │
│ Chain   │ libp2p  │ REST   │ Keygen │
│ Mempool │ Noise   │ JSONRPC│ AES-GCM│
│ Blocks  │ Yamux   │ Explor │ Argon2 │
│ Trie    │ Sync    │        │        │
│ VM      │         │        │        │
├─────────┴─────────┴────────┴────────┤
│           Storage (MDBX)            │
└──────────────────────────────────────┘
```

## Module Map

```
src/core/blockchain.rs       Chain state, genesis, constants
src/core/block_producer.rs   Block creation (coinbase + mempool txs)
src/core/block_executor.rs   Two-pass validate-then-commit
src/core/mempool.rs          Priority queue, per-sender caps, TTL
src/core/authority.rs        Validators, round-robin, admin audit log
src/core/transaction.rs      ECDSA signing, nonce, chain_id
src/core/account.rs          Balances (u64 sentri), fee split
src/core/trie/               State trie — 256-level Binary SMT
src/core/vm.rs               SRC-20 token engine
src/core/block.rs            Block struct, hashing
src/core/merkle.rs           SHA-256 tx merkle root
src/network/libp2p_node.rs   P2P swarm, broadcast, sync
src/network/behaviour.rs     Identify + RequestResponse
src/network/transport.rs     TCP → Noise XX → Yamux
src/network/sync.rs          Incremental block sync
src/api/routes.rs            REST (25+ endpoints), rate limiting
src/api/jsonrpc.rs           JSON-RPC 2.0 (20 methods)
src/api/explorer.rs          12-page block explorer
src/wallet/                  Keygen, keystore (Argon2id)
src/storage/db.rs            MDBX persistence, hash index
src/types/error.rs           SentrixError (14 variants)
```

## How Blocks Work

1. Validator checks if it's their turn: `height % validator_count`
2. Builds coinbase (1 SRX reward) + grabs up to 100 txs from mempool
3. Two-pass execution:
   - **Pass 1**: Validate everything on a copy — if anything fails, reject the whole block
   - **Pass 2**: Commit — credit coinbase, execute transfers, burn fees, update trie
4. Broadcast to peers

## Key Decisions

**Account model, not UTXO.** Simpler, natural fit for tokens and future smart contracts.

**MDBX for storage.** Memory-mapped B+ tree (used by Reth/Erigon), ACID transactions. Blocks stored as `block:{index}`, hash index for O(1) lookup.

**Sliding window.** Only last 1,000 blocks in RAM (~2 MB cap). Older blocks read from MDBX on demand.

**Integer-only balances.** Everything in sentri (1 SRX = 100M sentri). No floats, no rounding bugs.

## Stats

| | |
|-|-|
| Source files | ~50 |
| Lines of code | ~22,500+ |
| Tests | 551 |
| Workspace crates | 12 + 1 binary |
| `unsafe` blocks | 0 |
| License | BUSL-1.1 |
