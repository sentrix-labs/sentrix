# sentrix-nft

**Sentrix Native NFT — a Proof Asset layer.**

Pure NFT domain logic for Sentrix Chain. This crate is the native (SRC-721-style)
NFT standard used for **proofs and reputation**, not a JPEG/PFP marketplace.

## What it's for

- validator proof
- builder proof
- testnet-participation proof
- contributor proof
- bug-hunter proof
- genesis-supporter proof
- community reputation
- optional **soulbound** (non-transferable) assets

## What it is and isn't

- **Pure domain logic only.** Collections, tokens, ownership, approvals,
  soulbound/freeze rules, metadata URIs, deterministic ids, and events. No MDBX,
  no trie/state-root, no block execution, no RPC, no networking, no marketplace,
  no bridge.
- **Metadata lives off-chain.** Only URIs (`ipfs://`, `ar://`, `https://`, …)
  and optional integrity hashes are stored. **Image/media bytes are never stored
  in chain state.** No IPFS/Arweave upload logic lives here.
- **EVM NFTs are separate.** ERC-721/ERC-1155 via `eth_sendRawTransaction`
  remain a parallel path for public developers and normal collections. This
  native rail does not replace them.

## Determinism

- Collection ids: `SRC721_<sha256(creator|seed)[..20]>`. No wall-clock time, no
  randomness — every node derives the same id when applying the same block.
- Token ids: deterministic `u64`, chosen by the minter.
- **Strict no-reuse:** a burned token id is retired forever (kept as a
  tombstone). History stays append-only and auditable — important for a
  reputation layer.

## Model

- `NftRegistry` — holds every deployed collection.
- `NftCollection` — one collection: tokens, balances, approvals, admin, freeze
  and metadata-lock flags, `max_supply` (counts ever-minted).
- `NftToken` — one token: owner, optional URI + integrity hashes, `transferable`
  (false = soulbound), `frozen`, `burned`.
- `NftEvent` — every state change returns a typed event.
- `NftError` — typed errors (no stringly-typed failures).

## Examples

```rust
use sentrix_nft::NftRegistry;

let mut reg = NftRegistry::new();

// 1. Create a soulbound Builder Badge collection (default_transferable = false).
let (id, _) = reg
    .deploy_collection(
        "0xfoundation", "Builder Badge", "BUILD", "ipfs://badges/",
        None,   // unlimited
        false,  // soulbound by default
        true,   // metadata mutable
        "tx-001",
    )
    .unwrap();
let col = reg.get_collection_mut(&id).unwrap();

// 2. Mint a soulbound Builder Badge (admin = creator mints).
col.mint("0xfoundation", "0xbuilder", 1, "", None).unwrap();
assert_eq!(col.owner_of(1), Some(&"0xbuilder".to_string()));

// 3. Mint a transferable token (override the collection default).
col.mint("0xfoundation", "0xalice", 2, "", Some(true)).unwrap();

// 4. Transfer the transferable token.
col.transfer("0xalice", "0xalice", "0xbob", 2).unwrap();
assert_eq!(col.owner_of(2), Some(&"0xbob".to_string()));

// 5. Soulbound transfer fails.
assert!(col.transfer("0xbuilder", "0xbuilder", "0xbob", 1).is_err());
```

## Future work (not in this crate)

- MDBX / state integration and the storage-key scheme (documented in `lib.rs`).
- Trie / state-root commitment of NFT state.
- Native NFT transaction execution in `sentrix-core`'s block executor.
- JSON-RPC methods, explorer indexing, wallet display.
- ERC-721 compatibility adapter/precompile (EVM side).
