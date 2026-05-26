# sentrix-wire

[![crates.io](https://img.shields.io/crates/v/sentrix-wire.svg)](https://crates.io/crates/sentrix-wire)
[![docs.rs](https://docs.rs/sentrix-wire/badge.svg)](https://docs.rs/sentrix-wire)

Sentrix libp2p wire protocol types — request/response enums, gossipsub envelopes, protocol version + topic constants. No libp2p dependency.

## Why this crate exists

Wire types are part of the on-network protocol surface — every peer running a
Sentrix binary has to agree on the bincode layout of `SentrixRequest` /
`SentrixResponse` and the gossipsub topic names. Keeping those types in a crate
that depends only on [sentrix-primitives](../sentrix-primitives) +
[sentrix-bft](../sentrix-bft) (no libp2p, no async runtime, no framing) lets
downstream SDKs, monitoring tools, and light clients reference them without
pulling the full libp2p stack. The actual codec + behaviour lives in
[sentrix-network](../sentrix-network).

Stability rules: adding a variant bumps `SENTRIX_PROTOCOL` and rolls out
testnet-first; renaming / reordering variants is an immediate wire break
(bincode is position-dependent); removing a variant requires a hard fork at a
pinned height.

## Usage

```toml
[dependencies]
sentrix-wire = { path = "../sentrix-wire" }
```

```rust
use sentrix_wire::{
    SentrixRequest, SentrixResponse, MultiaddrAdvertisement,
    SENTRIX_PROTOCOL, BLOCKS_TOPIC, TXS_TOPIC,
    BFT_PROPOSAL_TOPIC, BFT_PREVOTE_TOPIC, BFT_PRECOMMIT_TOPIC,
    VALIDATOR_ADVERTS_TOPIC, MAX_MESSAGE_BYTES,
};

// Handshake on connection open — peers exchange chain_id for network partitioning.
let req = SentrixRequest::Handshake {
    host: "198.51.100.10".into(),
    port: 30303,
    height: 1_700_000,
    chain_id: 7119,
};

// Variant name for diagnostics / metrics labels.
assert_eq!(req.variant_name(), "Handshake");

// Current protocol identifier: `/sentrix/2.2.0`.
let _ = SENTRIX_PROTOCOL;
```

Also defines `MultiaddrAdvertisement` — the signed gossipsub message validators
broadcast for L1 peer auto-discovery, with cross-chain replay protection
(mainnet 7119 vs testnet 7120) baked into the signing payload.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
