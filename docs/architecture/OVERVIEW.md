# Architecture Overview

Sentrix is a Layer-1 blockchain written in Rust. **Voyager DPoS+BFT consensus** (live since 2026-04-25; Pioneer PoA round-robin was bootstrap consensus through h=579046), account-based model (like Ethereum), custom Binary Sparse Merkle Tree for state proofs, EVM execution via revm 37.

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

Sentrix is a Cargo workspace. Each top-level concern lives in its own
crate under `crates/`; the binary at `bin/sentrix/` wires them together.

```
crates/sentrix-primitives/      Block, Transaction, Account, Error types
crates/sentrix-codec/           Wire-format encoding helpers
crates/sentrix-wire/            Wire-protocol message types
crates/sentrix-wallet/          Keygen, Argon2id keystore
crates/sentrix-trie/            256-level Binary Sparse Merkle Tree
crates/sentrix-staking/         DPoS, epoch rotation, slashing
crates/sentrix-evm/             revm 37 adapter
crates/sentrix-precompiles/     EVM precompiles
crates/sentrix-bft/             BFT consensus (timeout-only round advance)
crates/sentrix-core/            Blockchain, authority, executor, mempool,
                                token engine, two-pass validator
crates/sentrix-network/         libp2p P2P, gossipsub, kademlia, sync
crates/sentrix-rpc/             REST (60+ endpoints) + JSON-RPC 2.0 + explorer
crates/sentrix-rpc-types/       Shared RPC request/response types
crates/sentrix-storage/         MDBX wrapper, ChainStorage API, hash index
bin/sentrix/src/main.rs         CLI entry point
```

## How Blocks Work

1. Validator checks if it's their turn: `height % validator_count`
2. Builds coinbase (1 SRX reward) + grabs up to 5,000 txs from mempool (sorted by fee, highest first)
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
| Lines of code | ~22,500+ |
| Tests | 551+ |
| Workspace crates | 14 + 1 binary |
| `unsafe` blocks | 0 |
| License | BUSL-1.1 |
