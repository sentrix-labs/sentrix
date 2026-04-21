# SRC-721 — Native NFT Spec

Sentrix adds a native NFT module (`SRC-721`) as a first-class chain
operation, mirroring the shape of the existing SRC-20 native tokens.
Users pick between native (`SRC-721`) for simple collections and EVM
(ERC-721 bytecode) for custom logic, same as the SRC-20 vs ERC-20
choice.

> Scope: behavior spec only. Implementation plan, risk analysis, and
> sequencing live in the internal companion doc.

---

## 1. Why native NFTs

Trade-off mirror of SRC-20:

|  | Native SRC-721 | EVM ERC-721 |
|---|---|---|
| Deploy cost | Hardcoded in chain — no bytecode upload | Full contract deployment |
| Per-op fee | Flat native fee (~50% cheaper than EVM) | EVM gas cost |
| Customisable logic | No (fixed schema) | Yes |
| Royalty / fee-on-transfer | Fixed policy per token at deploy | Arbitrary |
| Compatibility | Sentrix-native APIs + explorer | Full Ethereum ecosystem |

Target audience for native:
- **Indonesian loyalty / membership NFTs** (KTP-linked kelas, gym
  membership, event tickets) — simple, want low fees, don't need
  custom code.
- **In-app collectibles** (game, rewards, receipts) — tooling
  handles everything, wallet just needs to own the token.
- **Verifiable credentials** (certificates, diplomas).

Target audience for EVM:
- **OpenSea-style marketplaces** with complex royalty / auction
  logic.
- **Custom mint mechanics** (dutch auction, bonding curve, allow-list
  with on-chain merkle proofs).

---

## 2. Data model

### 2.1 Collection (one `SRC721_*` address per collection)

```rust
pub struct SRC721Collection {
    pub address: String,             // "SRC721_<hash>"
    pub name: String,                // 1..=64 chars
    pub symbol: String,              // 1..=10 ASCII alphanumeric
    pub max_supply: u64,             // 0 = unlimited
    pub total_minted: u64,
    pub total_burned: u64,
    pub deployer: String,            // 0x-prefixed address
    pub royalty_bps: u16,            // 0..=1000 (0-10%)
    pub royalty_recipient: String,   // where royalty goes on transfer
    pub base_uri: String,            // resolved per-token as `base_uri/{token_id}`
    pub frozen: bool,                // once true, no more mint
    pub created_height: u64,
}
```

### 2.2 Token (one entry per minted NFT)

```rust
pub struct SRC721Token {
    pub collection: String,          // SRC721 address
    pub token_id: u64,               // serial within collection
    pub owner: String,
    pub token_uri_override: Option<String>,  // overrides `base_uri/{id}` if set
    pub minted_height: u64,
    pub last_transfer_height: u64,
}
```

### 2.3 Approvals

```rust
pub struct SRC721Approval {
    pub collection: String,
    pub token_id: u64,       // specific token approval
    pub spender: String,     // who can transfer on owner's behalf
    pub expiry_height: u64,  // approval auto-expires
}

pub struct SRC721OperatorApproval {
    pub collection: String,
    pub owner: String,
    pub operator: String,    // approved for ALL tokens in this collection
}
```

---

## 3. Operations

### 3.1 Deploy collection

```rust
TokenOp::DeployNft {
    name: String,
    symbol: String,
    max_supply: u64,       // 0 = unlimited
    royalty_bps: u16,      // 0-1000
    royalty_recipient: String,
    base_uri: String,
}
```

Fee: same flat fee as SRC-20 deploy.

Emits: `NftCollectionDeployed { collection, deployer, name, symbol }`

### 3.2 Mint

```rust
TokenOp::MintNft {
    collection: String,
    to: String,
    token_id: u64,         // or None for auto-increment; see §4
    token_uri_override: Option<String>,
}
```

Only `deployer` or `operator` can mint until `frozen == true`.

Emits: `NftMinted { collection, token_id, to }`

### 3.3 Transfer

```rust
TokenOp::TransferNft {
    collection: String,
    token_id: u64,
    to: String,
}
```

Sender must be `owner`, approved spender, or operator. Royalty
deducted from transfer amount (if the transfer is paired with a
sale price in a future extension — v1 is a pure pointer change with
no attached value transfer).

Emits: `NftTransferred { collection, token_id, from, to }`

### 3.4 Burn

```rust
TokenOp::BurnNft {
    collection: String,
    token_id: u64,
}
```

Only `owner` can burn. Increments `total_burned`.

Emits: `NftBurned { collection, token_id, owner }`

### 3.5 Approve (single token)

```rust
TokenOp::ApproveNft {
    collection: String,
    token_id: u64,
    spender: String,        // empty = revoke
    expiry_height: u64,     // 0 = no expiry (valid until explicit revoke or transfer)
}
```

### 3.6 Approve operator (all tokens)

```rust
TokenOp::ApproveNftOperator {
    collection: String,
    operator: String,
    approved: bool,          // false = revoke
}
```

### 3.7 Freeze (one-way lock)

```rust
TokenOp::FreezeNftCollection {
    collection: String,
}
```

Only deployer. Once called, `max_supply` becomes a hard cap and no
new mints possible. Useful for final NFT drops where future mint
introduces rug risk.

### 3.8 Update `base_uri`

```rust
TokenOp::UpdateNftBaseUri {
    collection: String,
    base_uri: String,
}
```

Only deployer, only if not frozen.

---

## 4. Token ID allocation

Two modes:

- **Explicit**: caller passes `token_id`. Must not already exist.
  Fails otherwise.
- **Auto-increment**: caller passes `token_id: None`. Chain assigns
  next sequential `total_minted + 1`. Cheaper (no collision check),
  recommended for most cases.

Auto-increment is the default in the chain's deserialization —
`None` cheaper to encode than a full `u64`.

---

## 5. RPC endpoints (REST)

### 5.1 Collection queries

```
GET /nft/collections                       — list all SRC721 collections
GET /nft/collection/{addr}                 — collection metadata
GET /nft/collection/{addr}/tokens          — paginated tokens in collection
GET /nft/collection/{addr}/holders         — paginated holders + count per address
GET /nft/collection/{addr}/events          — paginated mint/transfer/burn events
```

### 5.2 Token queries

```
GET /nft/token/{collection_addr}/{token_id}            — token info
GET /nft/token/{collection_addr}/{token_id}/history    — full transfer history
```

### 5.3 Holder queries

```
GET /nft/address/{addr}                    — all SRC721 tokens owned by addr
GET /nft/address/{addr}/collections        — collections the addr holds
```

### 5.4 Approval queries

```
GET /nft/address/{addr}/approvals          — tokens addr has approved others to transfer
GET /nft/address/{addr}/operator-approvals — operators addr has approved
```

---

## 6. RPC endpoints (JSON-RPC, for tooling)

Under the `sentrix_` namespace:

- `sentrix_nftCollection(addr) → CollectionInfo`
- `sentrix_nftToken(addr, token_id) → TokenInfo | null`
- `sentrix_nftTokensOf(owner) → [Token]`
- `sentrix_nftCollectionsOf(owner) → [{collection, token_count}]`

These mirror the REST shape; exposed via JSON-RPC for indexers /
dApps that prefer one connection pattern.

---

## 7. Event log (for indexers)

Each op emits a structured event that goes into the block's event
log (same infrastructure as SRC-20 events):

```rust
pub enum NftEvent {
    CollectionDeployed { collection, deployer, name, symbol, base_uri },
    Minted { collection, token_id, to, minter },
    Transferred { collection, token_id, from, to },
    Burned { collection, token_id, owner },
    Approved { collection, token_id, owner, spender, expiry_height },
    OperatorApprovalChanged { collection, owner, operator, approved },
    CollectionFrozen { collection },
    BaseUriUpdated { collection, new_base_uri },
}
```

Indexers (Sentrix Scan NFT tab, subgraph-style services) consume
these directly instead of having to parse tx payloads.

---

## 8. Compatibility with EVM ecosystem

Native SRC-721 is **not** wire-compatible with ERC-721 tooling
(OpenSea reads ERC-721 contracts on EVM chains). Two paths:

1. **Native-only** — SRC-721 collection is only discoverable /
   transferable via Sentrix tooling. Fine for loyalty / in-app /
   Indonesia-native use cases.
2. **EVM bridge contract** (optional follow-up) — a wrapper ERC-721
   contract that holds native SRC-721 tokens in escrow and exposes
   a standard ERC-721 surface for OpenSea-style marketplaces. Users
   call `wrap(token_id)` to move a native token into the wrapper,
   `unwrap(token_id)` to get it back. Not in v1 scope.

For users who need OpenSea day-one, direct them to deploy ERC-721 on
Sentrix EVM. That already works (post-Voyager fork).

---

## 9. Fee model

Per-op flat fees (native-fee schedule, not gas):

| Op | Fee |
|---|---|
| DeployNft | 10 × `MIN_TX_FEE` (same as SRC-20 deploy) |
| MintNft | 1 × `MIN_TX_FEE` |
| TransferNft | 1 × `MIN_TX_FEE` |
| BurnNft | 1 × `MIN_TX_FEE` |
| ApproveNft | 0.5 × `MIN_TX_FEE` (metadata-only ops cheaper) |
| ApproveNftOperator | 0.5 × `MIN_TX_FEE` |
| FreezeNftCollection | 0 |
| UpdateNftBaseUri | 0.5 × `MIN_TX_FEE` |

50% of each fee is burned (same policy as SRC-20), 50% to proposer.

Royalties at transfer time: if the transfer includes attached value
(future extension, v1 is pure pointer), `royalty_bps` of that value
is sent to `royalty_recipient`. v1 doesn't attach value to transfers.

---

## 10. Rollout

Target: **post-Voyager mainnet fork**, after EIP-1559 activation.
Rationale:

1. NFT launch on mainnet is a user-facing product moment. Don't
   mix with consensus-critical work.
2. EIP-1559 needs to land first so fee dynamics are predictable for
   NFT mint events (avoid a pathological mint draining the mempool).
3. Reward-distribution-v2 needs to land first so NFT op fees flow
   through the new signer-proportional path.

Estimated calendar: **~6 weeks after Voyager mainnet fork** before
SRC-721 activates on mainnet.
