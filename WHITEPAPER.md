# Sentrix — Technical Whitepaper

**Version 3.1 — 2026-04-25**
**Author: SentrisCloud**

---

## Abstract

Sentrix is a Layer-1 blockchain engineered for fast, deterministic settlement. Built from scratch in Rust as a 14-crate workspace, it delivers 1-second blocks, Ethereum-compatible addressing, an MDBX-backed state layer, and a native fungible token standard (SRC-20). The chain runs a two-phase consensus design: **Pioneer** (Proof of Authority round-robin) on mainnet today, with **Voyager** (Delegated Proof of Stake + BFT finality + EVM execution) already live on testnet and pending mainnet activation.

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

## 2. Consensus: Proof of Authority

### 2.1 Validator Selection

Validators are authorized by the chain administrator. Each validator is identified by an Ethereum-compatible address derived from an ECDSA secp256k1 keypair.

### 2.2 Round-Robin Scheduling

Block production follows a deterministic round-robin schedule:

```
expected_producer = active_validators[block_height % validator_count]
```

Validators are sorted by address (ascending) to ensure all nodes agree on the schedule without communication overhead.

### 2.3 Block Time

Block time is **1 second** (`BLOCK_TIME_SECS = 1`). Each validator produces one block per round. Mainnet runs **4 active validators** in round-robin (`expected_producer = sorted_validators[height % 4]`), so each validator produces a block every 4 seconds. `MIN_ACTIVE_VALIDATORS = 1` keeps the chain advancing even when 3 of 4 validators are offline; `MIN_BFT_VALIDATORS = 4` is the BFT-quorum threshold under Voyager.

### 2.4 Finality

Blocks achieve **instant finality** upon production. There is no fork choice rule, no uncle blocks, and no reorganization. A block produced by the authorized validator is final.

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
| Maximum supply | 210,000,000 SRX |
| Smallest unit | 1 sentri = 10^-8 SRX |
| Genesis premine | 63,000,000 SRX (30%) |
| Block rewards | 147,000,000 SRX (70%) |

### 6.2 Premine Allocation

| Recipient | Amount | Share | Purpose |
|---|---|---|---|
| Founder | 21,000,000 SRX | 10% | Operations, treasury |
| Ecosystem Fund | 21,000,000 SRX | 10% | Development grants, partnerships |
| Early Validators | 10,500,000 SRX | 5% | Validator incentives |
| Reserve | 10,500,000 SRX | 5% | Emergency, unforeseen needs |

### 6.3 Block Rewards

`HALVING_INTERVAL = 42_000_000` blocks. At 1-second block time, each era spans 42M seconds ≈ **1.33 years**.

| Era | Block Range | Reward | Duration (~) |
|---|---|---|---|
| 0 | 0 — 41,999,999 | 1 SRX | ~1.33 years |
| 1 | 42,000,000 — 83,999,999 | 0.5 SRX | ~1.33 years |
| 2 | 84,000,000 — 125,999,999 | 0.25 SRX | ~1.33 years |
| 3 | 126,000,000+ | 0.125 SRX | ~1.33 years |
| ... | ... | halves | ... |

Rewards are clamped to the remaining supply headroom. Once `total_minted == MAX_SUPPLY`, block rewards become zero.

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
| **SRX** | Native coin | 210,000,000 (fixed) | Gas fees, validator rewards, base currency, store of value |
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
| **Pioneer (PoA)** | LIVE | PoA round-robin engine, MDBX state, SentrixTrie, libp2p networking, SRC-20, JSON-RPC, security audits V1–V11 |
| **Voyager (DPoS+BFT+EVM)** | TESTNET LIVE / MAINNET PENDING | DPoS staking, BFT finality, EVM execution. Active on testnet since 2026-04-23. Mainnet activation pending V2 main.rs wiring (GitHub #292). |
| **Frontier** | FUTURE | dApp ecosystem expansion, real-user scaling, validator decentralization |
| **Odyssey** | FUTURE | Cross-chain bridges, mature ecosystem, full public chain |

---

## 13. Current State (2026-04-25)

| Item | Value |
|---|---|
| Mainnet binary | v2.1.25 |
| Testnet binary | v2.1.24 |
| Mainnet height | ~558,000+ |
| Mainnet block time | 1 second |
| Mainnet validators | 4 active (Foundation, Treasury, Core, Beacon) on Pioneer round-robin |
| Mainnet mode | Pioneer with `SENTRIX_FORCE_PIONEER_MODE=1` emergency override (Voyager activation rolled back same-day; tracked in GitHub #292) |
| Testnet | 4 validators, Voyager DPoS+BFT+EVM ACTIVE since 2026-04-23 docker migration, fresh genesis, h~200K |
| Workspace | 14 Rust crates (`crates/sentrix-*`) + binary at `bin/sentrix/src/main.rs` |
| Storage backend | MDBX (libmdbx) |
| Tests | 500+ unit + 16 integration |

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

- Explorer: sentrixscan.sentriscloud.com
- API: sentrix-api.sentriscloud.com
- RPC: sentrix-rpc.sentriscloud.com
- GitHub: github.com/sentrix-labs/sentrix
- Email: sentriscloud@gmail.com

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
