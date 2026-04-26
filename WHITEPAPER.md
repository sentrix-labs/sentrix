# Sentrix — Technical Whitepaper

**Version 3.4 — 2026-04-26 (post tokenomics-v2 fork; max supply 315M, BTC-parity 4-year halving)**
**Author: Sentrix Labs** (operated by SentrisCloud)

---

## Abstract

Sentrix is a Layer-1 blockchain engineered for fast, deterministic settlement. Built from scratch in Rust as a 14-crate workspace, it delivers 1-second blocks, Ethereum-compatible addressing, an MDBX-backed state layer, and a native fungible token standard (SRC-20). The chain transitioned through a two-phase consensus design: **Pioneer** (Proof of Authority round-robin) bootstrapped the network from genesis, and **Voyager** (Delegated Proof of Stake + BFT finality + EVM execution) is now active on both mainnet (since 2026-04-25, h=579047) and testnet.

---

## 1. Introduction

### 1.1 Problem Statement

Existing blockchain platforms force developers to choose between decentralization, performance, and simplicity:

- **Bitcoin/Ethereum PoW**: Decentralized but slow (10min / 12sec blocks) and energy-intensive
- **Ethereum PoS**: Improved speed but complex validator economics and high gas fees
- **Solana**: High performance but frequent outages and complex architecture
- **BSC/Polygon**: Fast but often criticized for centralization

There is a need for a blockchain that starts simple and controlled, then progressively decentralizes — allowing the ecosystem to grow organically without premature complexity.

### 1.2 Solution

Sentrix takes the **progressive decentralization** approach:

1. **Pioneer**: Proof of Authority — fast, controlled, battle-tested
2. **Voyager**: DPoS + BFT + EVM — public validators, smart contracts
3. **Frontier**: Ecosystem expansion — dApps, real users, scaling
4. **Odyssey**: Full public chain — cross-chain bridges, mature ecosystem

This mirrors the path taken by successful chains like BNB Chain (PoA → DPoS) and Polygon (PoA → PoS).

---

## 2. Consensus

> **Mainnet status (2026-04-25 onward):** mainnet runs Voyager (DPoS + BFT finality). The Pioneer PoA design described in §2.1–§2.4 below is retained for historical reference — it is the consensus engine that produced blocks 0…579046 before the Voyager fork. Voyager's DPoS proposer rotation + 3-phase BFT (Propose → Prevote → Precommit) is the active engine; see §2.5 for the live design.

### 2.1 Validator Selection (Pioneer, historical)

Validators are authorized by the chain administrator. Each validator is identified by an Ethereum-compatible address derived from an ECDSA secp256k1 keypair.

### 2.2 Round-Robin Scheduling (Pioneer, historical)

Block production follows a deterministic round-robin schedule:

```
expected_producer = active_validators[block_height % validator_count]
```

Validators are sorted by address (ascending) to ensure all nodes agree on the schedule without communication overhead.

### 2.3 Block Time

Block time is **1 second** (`BLOCK_TIME_SECS = 1`). Each validator produces one block per round. Mainnet runs **4 active validators** in round-robin (`expected_producer = sorted_validators[height % 4]`), so each validator produces a block every 4 seconds. `MIN_ACTIVE_VALIDATORS = 1` keeps the chain advancing even when 3 of 4 validators are offline; `MIN_BFT_VALIDATORS = 4` is the BFT-quorum threshold under Voyager.

### 2.4 Pioneer Finality (historical)

Under Pioneer, blocks achieved **instant finality** upon production. There was no fork choice rule, no uncle blocks, and no reorganization. A block produced by the authorized validator was final by construction.

### 2.5 Voyager — DPoS + BFT (active mainnet engine)

Voyager replaces Pioneer's authority-based round-robin with a stake-weighted active set + 3-phase BFT vote protocol:

- **Validator set selection.** Open registration with a 15,000 SRX self-stake floor. Top 100 validators by stake score (self-stake + delegations × commission factor) form the `active_set`. Epoch rotation evicts non-performers.
- **Proposer rotation.** Same `active_set[height % len()]` deterministic rule as Pioneer, applied over the live stake-ranked set instead of the admin-curated set.
- **3-phase BFT round.** Propose → Prevote → Precommit. A block is committed when ≥ 2/3+1 of stake-weighted precommits are gathered. Locked-block-repropose handles partial-supermajority cases without forking.
- **Justifications.** Each committed block carries a `justification` field with the precommit signatures that finalised it. `sentrix_getFinalizedHeight` returns the height of the newest justified block; light clients verify finality by checking justifications against the on-chain stake registry.
- **Slashing.** Double-sign and prolonged offline trigger automatic stake slashing under `crates/sentrix-staking/`. Slash evidence is submitted on-chain via the `SubmitEvidence` staking op.
- **EVM gating.** Voyager activation also flips `evm_activated=true`, enabling `eth_sendRawTransaction` and Solidity contract deployment via revm 37.

Voyager activated on mainnet at h=579047 on 2026-04-25 after Pioneer ran from genesis through h=579046.

### 2.7 V4 Reward Distribution (active mainnet)

Pre-V4 (Pioneer + early Voyager): coinbase 1 SRX/block credited directly to the proposing validator's balance.

Post-V4 (active on mainnet since h=590100, 2026-04-25): coinbase routes to `PROTOCOL_TREASURY` (`0x0000000000000000000000000000000000000002`). Validators and their delegators accumulate stake-weighted shares in `pending_rewards` accumulators. Funds are released to claimers via the `StakingOp::ClaimRewards` transaction:

```
Pre-V4:  block produced → coinbase 1 SRX → validator balance
Post-V4: block produced → coinbase 1 SRX → PROTOCOL_TREASURY
                                            ↓ (escrow)
                          ClaimRewards tx ← validator/delegator
                                            ↓
                          PROTOCOL_TREASURY → claimer balance
                          pending_rewards reset to 0
```

Why the change:
- Stake-weighted delegation share — delegators earn pro-rata from validator's commission carve-out without manual accounting
- Slashing applies to pending_rewards before claim — validator misbehavior reduces accumulated reward, not yet-paid balance
- On-chain audit trail — `pending_rewards` per validator is queryable via `/staking/validators` JSON-RPC endpoint
- Treasury fund growth visible — total `PROTOCOL_TREASURY` balance reflects unclaimed escrow at any height

The activation was a non-fork operator-side env var flip (`VOYAGER_REWARD_V2_HEIGHT=590100`); code path was already staged in v2.1.30+ binaries. At fork height, `reset_reward_accumulators_for_fork_activation()` fired once to clear pre-V4 accumulator state, then the new path took effect for all subsequent blocks.

### 2.6 Peer Mesh (L1 + L2 self-healing)

DPoS+BFT consensus assumes the validator mesh is well-connected. Sentrix ships two layers of self-healing peer discovery:

- **L1 multiaddr advertisements.** Validators broadcast signed `MultiaddrAdvertisement` messages on the `sentrix/validator-adverts/1` gossipsub topic at startup + every 10 minutes. Receivers verify against on-chain stake registry pubkeys and store latest-by-sequence in a 4096-entry LRU cache. A periodic dial-tick (every 30s) reads `active_set` and dials any cached members not currently peered. Sequence numbers are persisted to `<data_dir>/.advert-sequence` so restarts don't reset the newer-wins ordering.
- **L2 cold-start gate.** The validator loop refuses to enter BFT mode unless `peer_count ≥ active_set.len() − 1`. The gate fires every loop iteration when `voyager_activated=true` (not only on activation transitions), closing the cold-start race where a validator restarting with `voyager_activated=true` already in chain.db could enter BFT before the L1 mesh re-converges. Strict `SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=="1"` env override exists for emergency recovery.

Together these guarantee a fresh validator joining from a single bootstrap peer converges to the full mesh within ~30s without manual `--peers` configuration.

---

## 3. Account Model

### 3.1 State

Sentrix uses an Ethereum-style account model. Each address maps to:

| Field | Type | Description |
|---|---|---|
| `balance` | u64 | SRX balance in sentri (1 SRX = 10^8 sentri) |
| `nonce` | u64 | Transaction counter (replay protection) |

### 3.2 Address Format

Addresses follow the Ethereum standard:
1. Generate ECDSA secp256k1 keypair
2. Take uncompressed public key (64 bytes, exclude 0x04 prefix)
3. Keccak-256 hash
4. Take last 20 bytes
5. Prefix with `0x`

Result: `0x` + 40 hexadecimal characters (e.g., `0x89639929a133562d871dd47304ad3ff597908b79`)

This ensures compatibility with Ethereum wallets, block explorers, and developer tools.

---

## 4. Transactions

### 4.1 Structure

| Field | Type | Description |
|---|---|---|
| `txid` | SHA-256 hash | Unique transaction identifier |
| `from_address` | string | Sender address |
| `to_address` | string | Recipient address |
| `amount` | u64 | Transfer amount (sentri) |
| `fee` | u64 | Transaction fee (sentri) |
| `nonce` | u64 | Sender's current nonce |
| `data` | string | Arbitrary data field |
| `timestamp` | u64 | Unix timestamp |
| `chain_id` | u64 | Chain identifier (replay protection) |
| `signature` | hex | ECDSA signature |
| `public_key` | hex | Sender's public key |

### 4.2 Signing

Transactions are signed using ECDSA over the secp256k1 curve:

1. Construct canonical JSON payload (sorted keys, deterministic)
2. SHA-256 hash the payload
3. Sign the hash with the sender's private key
4. Attach signature and public key to transaction

### 4.3 Validation

Each transaction must satisfy:
- Valid ECDSA signature
- Correct nonce (sequential, no gaps)
- Correct chain_id (must match network — prevents cross-chain replay attacks)
- Fee ≥ minimum (0.0001 SRX)
- Amount > 0
- Sender balance ≥ amount + fee
- All arithmetic uses checked operations (no integer overflow/underflow)

### 4.4 Mempool

Pending transactions are held in a priority queue ordered by fee (descending). Validators select the highest-fee transactions first, up to `MAX_TX_PER_BLOCK = 5000` per block.

---

## 5. Block Structure

| Field | Type | Description |
|---|---|---|
| `index` | u64 | Block height |
| `previous_hash` | SHA-256 | Link to parent block |
| `transactions` | Vec | Ordered list of transactions |
| `timestamp` | u64 | Block creation time |
| `merkle_root` | SHA-256 | Merkle root of transaction IDs |
| `validator` | string | Producer address |
| `hash` | SHA-256 | Block hash |

### 5.1 Block Hash

```
hash = SHA-256(index || previous_hash || merkle_root || timestamp || validator)
```

### 5.2 Merkle Tree

Transaction integrity is verified via a binary SHA-256 Merkle tree. Odd-count levels duplicate the last element before pairing.

### 5.3 Two-Pass Atomic Validation

Block application uses a two-pass protocol:

**Pass 1 (Dry Run):** Execute all transactions against a working state copy. If any transaction fails validation, the entire block is rejected.

**Pass 2 (Commit):** Apply all state changes atomically — balance transfers, fee splits, nonce increments, and supply adjustments.

This guarantees no partial state corruption under any failure condition.

---

## 6. Tokenomics

### 6.1 Supply

| Parameter | Value |
|---|---|
| Maximum supply | 315,000,000 SRX (post tokenomics-v2 fork) |
| Smallest unit | 1 sentri = 10^-8 SRX |
| Genesis premine | 63,000,000 SRX (20%) |
| Block rewards | 252,000,000 SRX (80%) |

> **Tokenomics v2 fork (2026-04-26):** Sentrix's emission curve was re-targeted to BTC-parity 4-year halving cadence. The v1 schedule (1 SRX × 42M halving) had a math gap — geometric series asymptoted at 84M from mining + 63M premine = 147M effective max, not the 210M originally documented. The v2 fork (`TOKENOMICS_V2_HEIGHT` env-gated, `MAX_SUPPLY_V2 = 315M`, `HALVING_INTERVAL_V2 = 126M blocks`) closes that gap: 1 SRX × 126M × 2 = 252M from mining + 63M premine = 315M cap (reachable). Side benefit: validator runway extended to ~year 20, premine ratio drops 30% nominal → 20% (industry-leading optics, lower than Solana 38% / Aptos 52% / Sui 58%).

### 6.2 Premine Allocation

| Recipient | Amount | Share (of 315M) | Purpose |
|---|---|---|---|
| Founder | 21,000,000 SRX | 6.67% | Operations, treasury |
| Ecosystem Fund | 21,000,000 SRX | 6.67% | Development grants, partnerships |
| Early Validators | 10,500,000 SRX | 3.33% | Validator incentives |
| Reserve | 10,500,000 SRX | 3.33% | Emergency, unforeseen needs |
| **Total premine** | **63,000,000 SRX** | **20%** | |

### 6.3 Block Rewards

Tokenomics v2 schedule (active): `HALVING_INTERVAL_V2 = 126_000_000` blocks. At 1-second block time, each era spans 126M seconds ≈ **4 years** (Bitcoin-parity halving cadence).

| Era | Block Range (post-fork-relative) | Reward | Duration (~) |
|---|---|---|---|
| 0 | 0 — 125,999,999 | 1 SRX | ~4 years |
| 1 | 126,000,000 — 251,999,999 | 0.5 SRX | ~4 years |
| 2 | 252,000,000 — 377,999,999 | 0.25 SRX | ~4 years |
| 3 | 378,000,000+ | 0.125 SRX | ~4 years |
| ... | ... | halves | ... |

Block ranges are computed relative to the fork height (`TOKENOMICS_V2_HEIGHT`); pre-fork blocks (history before fork activation) used the v1 42M-block schedule. At ~era 26 (year ~104), block reward integer-truncates to zero sentri — emission ends. Rewards are clamped to remaining supply headroom; once `total_minted == MAX_SUPPLY_V2` (315M), block rewards become zero.

> **First halving:** 4 years post-fork-activation. With fork activated at 2026-04-26, first halving lands ~2030.

### 6.4 Fee Economics

Every transaction fee is split:
- **50% permanently burned** (`burn_fee_share = total_fee.div_ceil(2)`) — creates deflationary pressure
- **50% to the block validator** — incentivizes block production

As network activity grows, burn rate increases. Eventually, burn rate exceeds block reward rate, causing the circulating supply to **decrease over time**.

---

## 7. SRC-20 Token Standard

SRC-20 is Sentrix's native fungible token standard, modeled after ERC-20.

### 7.1 Interface

| Method | Description |
|---|---|
| `mint(to, amount)` | Create new tokens (owner only) |
| `transfer(to, amount)` | Send tokens |
| `approve(spender, amount)` | Set spending allowance |
| `transfer_from(from, to, amount)` | Spend approved tokens |
| `balance_of(address)` | Query token balance |
| `allowance(owner, spender)` | Query spending allowance |

### 7.2 Deployment

Token deployment requires a fee paid in SRX, split 50/50 between burn and the ecosystem fund. The total supply is minted to the deployer upon creation.

### 7.3 Gas Model

Token operations (transfers, approvals) require a gas fee paid in **SRX** (not in the token itself). Gas fees follow the same 50/50 split: validator reward and permanent burn.

### 7.4 Token Burn

Any token holder can burn their tokens, permanently removing them from circulation:
- Reduces holder's balance by the burned amount
- Reduces total supply by the burned amount
- Requires SRX gas fee (50/50 split)

### 7.5 Three-Token Economy

Sentrix operates a three-token model:

| Token | Type | Supply | Role |
|---|---|---|---|
| **SRX** | Native coin | 315,000,000 (fixed, post tokenomics-v2 fork) | Gas fees, validator rewards, base currency, store of value |
| **SNTX** | SRC-20 | 10,000,000,000 | Utility — ecosystem rewards, governance voting, staking incentives |
| **SRTX** | SRC-20 | TBD | Payment — stablecoin for daily transactions |

**Flywheel effect:** Every SNTX and SRTX transaction requires SRX for gas. As token usage grows, more SRX is burned, increasing scarcity and value of SRX. This creates a positive feedback loop where ecosystem growth directly benefits the native coin holders.

---

## 8. Cryptographic Primitives

| Function | Algorithm | Standard |
|---|---|---|
| Transaction signing | ECDSA | secp256k1 (SEC 2) |
| Block hashing | SHA-256 | FIPS 180-4 |
| Address derivation | Keccak-256 | FIPS 202 |
| Merkle tree | SHA-256 | - |
| Wallet encryption | AES-256-GCM | NIST SP 800-38D |
| Key derivation | PBKDF2-HMAC-SHA-256 (600k iterations) | RFC 8018 |
| Memory safety | Private key zeroization on drop | - |
| Random generation | OS CSPRNG | - |

---

## 9. Network Architecture

### 9.1 Protocol

Nodes communicate over **libp2p** with Noise XX encryption and Yamux multiplexing. Wire format is `bincode` (replaced JSON for ~3-5× smaller messages). Protocol version: `/sentrix/2.0.0`. Maximum RequestResponse payload: 10 MiB.

### 9.2 Message Types

| Message | Direction | Purpose |
|---|---|---|
| Handshake / Identify | Bidirectional | Peer introduction, chain_id verification, height exchange |
| NewBlock | Gossipsub broadcast | Propagate produced blocks (`sentrix/blocks/1`) |
| NewTransaction | Gossipsub broadcast | Propagate pending transactions (`sentrix/txs/1`) |
| GetBlocks / BlocksResponse | Request-Response | Range sync, capped at 50 blocks per batch |
| Kademlia DHT | Background | Peer discovery |

### 9.3 Chain Synchronization

New nodes sync using **range-based RequestResponse** (50 blocks per batch) backed by **Kademlia DHT** for peer discovery. Each batch is validated against the local state via the same two-pass atomic protocol as live block application — no separate "sandbox" path. This guarantees that a malicious peer cannot fast-forward a node into an inconsistent state.

Per-IP rate limiting: 5 connections / IP / 60 seconds, 5-minute ban; max 50 peers per node.

---

## 10. Storage

Sentrix uses **libmdbx**, a memory-mapped B+ tree database (used by Reth and Erigon). MDBX replaced the original sled backend in v2.0.0 (the dedicated `sentrix-storage` crate wraps the C library with a Rust-safe `WriteBatch` and `NoWriteMap` mode).

### 10.1 Schema

| Key | Value |
|---|---|
| `state` | Blockchain state (accounts, authority, contracts, mempool) |
| `block:{N}` | Individual block at height N |
| `height` | Current chain height |
| `trie_nodes` | SentrixTrie internal + leaf nodes |
| `trie_values` | Account values keyed by 256-bit path |
| `trie_roots` | Committed state root per block height |
| `trie_committed_roots` | Reverse index NodeHash → version |

Per-block storage enables efficient single-block reads without loading the entire chain. Account state is committed into a **256-level Binary Sparse Merkle Tree** (BLAKE3 leaves, SHA-256 internal nodes, domain-separated). The state root is folded into the block hash from `STATE_ROOT_FORK_HEIGHT = 100_000` onwards.

---

## 11. API Compatibility

Sentrix exposes an Ethereum-compatible JSON-RPC 2.0 interface supporting 20 standard methods. This enables direct integration with:

- **MetaMask** — wallet connection and transaction signing
- **ethers.js / web3.js** — DApp development
- **Hardhat** — smart contract development and testing
- **Block explorers** — third-party chain analysis

Chain ID `7119` (`0x1bcf`) is registered for Sentrix mainnet; `7120` is the testnet chain ID.

---

## 12. Roadmap

| Phase | Status | Key Features |
|---|---|---|
| **Pioneer (PoA)** | COMPLETED (h=0…579046) | PoA round-robin engine, MDBX state, SentrixTrie, libp2p networking, SRC-20, JSON-RPC, security audits V1–V11. Succeeded by Voyager 2026-04-25. |
| **Voyager (DPoS+BFT+EVM)** | **LIVE — mainnet & testnet** | DPoS staking, BFT finality, EVM execution. Active on testnet since 2026-04-23. Mainnet active since 2026-04-25 at h=579047 with `voyager_activated=true` and `evm_activated=true`. L1 peer auto-discovery + L2 cold-start gate self-heal the validator mesh. |
| **Frontier** | SCAFFOLDED (Phase F-1 in main); F-2…F-10 planned | Mainnet hard fork to introduce parallel transaction execution, sub-1s block time, ecosystem expansion. Implementation tracked in `audits/frontier-mainnet-phase-implementation-plan.md`. |
| **Odyssey** | FUTURE | Cross-chain bridges, mature ecosystem, full public chain |

---

## 13. Current State (2026-04-25, post-Voyager mainnet activation)

| Item | Value |
|---|---|
| Mainnet binary | v2.1.30 |
| Testnet binary | v2.1.30 |
| Mainnet height | ~580,000+ |
| Mainnet block time | 1 second |
| Mainnet validators | 4 active (Foundation, Treasury, Core, Beacon) — DPoS proposer rotation under BFT finality |
| Mainnet consensus | **Voyager** (`consensus_mode="voyager"`, `voyager_activated=true`, `evm_activated=true`). Activated 2026-04-25 at h=579047 after the second activation attempt converged successfully. |
| Mainnet RPC | Reports `consensus: "DPoS+BFT"` on `/sentrix_status` and `/chain/finalized-height`; `chain_stats()` exposes `consensus_mode`, `voyager_activated`, `evm_activated` flags. |
| Testnet | 4 validators, Voyager DPoS+BFT+EVM active since 2026-04-23 docker migration, fresh genesis, h ~200K. |
| Workspace | 14 Rust crates (`crates/sentrix-*`) + binary at `bin/sentrix/src/main.rs` |
| Storage backend | MDBX (libmdbx) |
| Tests | 551+ unit + 16+ integration |

---

## 14. Funding & Development

### 14.1 Funding Status

Sentrix is currently self-funded by SentrisCloud. The project is bootstrapped — no external venture capital or token sale has occurred.

External investment and partnership opportunities will be announced through official channels at sentrix.io when the time is right.

### 14.2 Developer Grants

SentrisCloud allocates **21,000,000 SRX** from the Ecosystem Fund for developer grants. Grants are available for:

- Applications built on Sentrix
- Developer tooling (SDKs, libraries, integrations)
- Community infrastructure (wallets, explorers, bridges)
- Security research and auditing

To apply for a grant, reach out through official channels.

### 14.3 Bug Bounty

Security researchers who responsibly disclose vulnerabilities will be rewarded in SRX:

| Severity | Reward |
|---|---|
| Critical | Up to 100,000 SRX |
| High | Up to 50,000 SRX |
| Medium | Up to 10,000 SRX |
| Low | Up to 1,000 SRX |

See [SECURITY.md](SECURITY.md) for responsible disclosure policy.

### 14.4 Official Channels

- Explorer: scan.sentrixchain.com
- API: api.sentrixchain.com
- RPC: rpc.sentrixchain.com
- GitHub: github.com/sentrix-labs/sentrix
- Email: operator@sentriscloud.com

---

## 15. Conclusion

Sentrix demonstrates that a production-quality blockchain can be built with a lean codebase, clear design principles, and a progressive decentralization strategy. By starting with Proof of Authority and evolving toward Delegated Proof of Stake, Sentrix balances the practical needs of early-stage networks with the long-term goal of trustless, permissionless operation.

The combination of Rust's performance guarantees, Ethereum-compatible tooling, and a deflationary token model positions Sentrix as a viable foundation for application-specific blockchain ecosystems.

---

## 16. References

1. Nakamoto, S. (2008). *Bitcoin: A Peer-to-Peer Electronic Cash System*
2. Buterin, V. (2014). *Ethereum: A Next-Generation Smart Contract and Decentralized Application Platform*
3. NIST SP 800-38D. *Recommendation for Block Cipher Modes of Operation: Galois/Counter Mode (GCM)*
4. SEC 2: *Recommended Elliptic Curve Domain Parameters* (secp256k1)
5. RFC 8018: *PKCS #5: Password-Based Cryptography Specification Version 2.1*

---

*Copyright 2026 SentrisCloud. All rights reserved.*
*Licensed under BUSL-1.1. See LICENSE for details.*
