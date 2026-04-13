# Sentrix — Technical Whitepaper

**Version 3.0 — April 2026**
**Author: SentrisCloud**

---

## Abstract

Sentrix is a Layer-1 Proof-of-Authority blockchain engineered for fast, deterministic settlement. Built from scratch in Rust, it delivers 3-second block finality, Ethereum-compatible addressing, and a native fungible token standard (SRX-20). The chain is designed to evolve from a permissioned PoA network into a fully decentralized public chain through a phased transition to Delegated Proof of Stake.

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

1. **Phase 1**: Proof of Authority — fast, controlled, battle-tested
2. **Phase 2**: Public PoA — open participation, token economy
3. **Phase 3**: Delegated Proof of Stake — community-governed validators
4. **Phase 4**: Full public chain — smart contracts, cross-chain bridges

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

Target block time is **3 seconds**. Each validator produces one block per round. With N validators, each validator produces a block every 3N seconds.

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

Pending transactions are held in a priority queue ordered by fee (descending). Validators select the highest-fee transactions first, up to 100 per block.

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

| Era | Block Range | Reward | Duration (~) |
|---|---|---|---|
| 0 | 0 — 41,999,999 | 1 SRX | ~4 years |
| 1 | 42,000,000 — 83,999,999 | 0.5 SRX | ~4 years |
| 2 | 84,000,000 — 125,999,999 | 0.25 SRX | ~4 years |
| 3 | 126,000,000+ | 0.125 SRX | ~4 years |
| ... | ... | halves | ... |

Rewards are clamped to the remaining supply headroom. Once `total_minted == MAX_SUPPLY`, block rewards become zero.

### 6.4 Fee Economics

Every transaction fee is split:
- **50% to the block validator** — incentivizes block production
- **50% permanently burned** — creates deflationary pressure

As network activity grows, burn rate increases. Eventually, burn rate exceeds block reward rate, causing the circulating supply to **decrease over time**.

---

## 7. SRX-20 Token Standard

SRX-20 is Sentrix's native fungible token standard, modeled after ERC-20.

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
| **SNTX** | SRX-20 | 10,000,000,000 | Utility — ecosystem rewards, governance voting, staking incentives |
| **SRTX** | SRX-20 | TBD | Payment — stablecoin for daily transactions |

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

Nodes communicate via TCP using a length-prefixed JSON protocol:

```
[4 bytes: payload length (big-endian)] [JSON payload]
```

Maximum message size: 10 MB.

### 9.2 Message Types

| Message | Direction | Purpose |
|---|---|---|
| Handshake | Bidirectional | Peer introduction, height exchange |
| NewBlock | Broadcast | Propagate produced blocks |
| NewTransaction | Broadcast | Propagate pending transactions |
| GetChain / ChainResponse | Request-Response | Full chain synchronization |
| GetHeight / HeightResponse | Request-Response | Quick height check |
| Ping / Pong | Health check | Liveness monitoring |

### 9.3 Chain Synchronization

New nodes sync using a **sandbox validation** protocol:

1. Request full chain from a peer
2. Validate chain structure (hash links, block integrity)
3. Replay all blocks in a sandbox Blockchain instance
4. If all blocks pass validation, replace local state
5. Persist to storage

This prevents accepting invalid or malicious chains.

---

## 10. Storage

Sentrix uses **sled**, a pure-Rust embedded key-value database.

### 10.1 Schema

| Key | Value |
|---|---|
| `state` | Blockchain state (accounts, authority, contracts, mempool) |
| `block:{N}` | Individual block at height N |
| `height` | Current chain height |

Per-block storage enables efficient single-block reads without loading the entire chain.

---

## 11. API Compatibility

Sentrix exposes an Ethereum-compatible JSON-RPC 2.0 interface supporting 20 standard methods. This enables direct integration with:

- **MetaMask** — wallet connection and transaction signing
- **ethers.js / web3.js** — DApp development
- **Hardhat** — smart contract development and testing
- **Block explorers** — third-party chain analysis

Chain ID `7119` (`0x1bcf`) is registered for Sentrix.

---

## 12. Roadmap

| Phase | Target | Key Features |
|---|---|---|
| **1** ✅ | 2026 Q2 | PoA engine, wallets, SRX-20, explorer, JSON-RPC |
| **2** ✅ | 2026 Q2 | Full P2P, security audit, three-token model, block explorer |
| **3** | 2026 Q3-Q4 | Public mainnet, multi-node deployment, wallet web UI |
| **4** | 2027 | DPoS transition, staking, governance |
| **5** | 2027-2028 | Smart contract VM, SDKs, cross-chain bridge, mobile wallet |

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
- GitHub: github.com/satyakwok/sentrix
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
