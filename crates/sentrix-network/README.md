# sentrix-network

[![crates.io](https://img.shields.io/crates/v/sentrix-network.svg)](https://crates.io/crates/sentrix-network)
[![docs.rs](https://docs.rs/sentrix-network/badge.svg)](https://docs.rs/sentrix-network)

P2P networking for Sentrix Chain — libp2p gossipsub + kademlia + request-response.

## Why this crate exists

The validator binary needs a peer-to-peer transport for block propagation, transaction
gossip, BFT vote dissemination, and chain sync. This crate owns the libp2p
`NetworkBehaviour` composition, the Kademlia DHT for peer discovery, gossipsub topics
for blocks / txs / BFT phases, and the request-response codec used for `GetBlocks`
and handshakes.

Wire-format types (request/response enums, gossipsub envelopes, protocol version
string, topic constants) live in [sentrix-wire](../sentrix-wire) so SDKs and monitoring
tools can reference them without pulling libp2p as a transitive dep. This crate brings
in `sentrix-wire` + [sentrix-bft](../sentrix-bft) + [sentrix-core](../sentrix-core) to
turn those types into an actual node.

## Usage

```toml
[dependencies]
sentrix-network = { path = "../sentrix-network" }
```

```rust
use sentrix_network::{LibP2pNode, NodeEvent};
use tokio::sync::mpsc;

// Validator main loop owns the receiver; LibP2pNode is constructed with the
// matching sender so the swarm task can forward inbound NodeEvents
// (NewBlock, NewTransaction, BftProposal/Prevote/Precommit) into the engine.
let (event_tx, mut event_rx) = mpsc::channel::<NodeEvent>(4096);
let _node = LibP2pNode::new(/* keypair, blockchain, event_tx, ... */).await?;

while let Some(event) = event_rx.recv().await {
    // dispatch NodeEvent::NewBlock / NewTransaction / Bft* to the engine
}
```

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
