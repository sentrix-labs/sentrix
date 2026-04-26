# Networking

All P2P communication goes through libp2p. Transport stack: TCP → Noise XX (encrypted, mutual auth) → Yamux (multiplexed streams).

## Transport

```rust
// transport.rs — simplified
tcp::Config::default().nodelay(true)  // low latency
→ noise::Config::new                  // Noise XX handshake
→ yamux::Config::default              // stream multiplexing
```

Noise XX gives mutual authentication (both sides prove identity via Ed25519 keypair) and forward secrecy. No certificates needed.

## Node Identity

Each node has an Ed25519 keypair at `data/identity/node_keypair`, generated on first run. PeerId = hash of public key. Stays the same across restarts even if IP changes.

## Peer Discovery

Two mechanisms:

1. **Kademlia DHT** — automatic peer discovery via random walks every 60s. When Identify completes, peer listen addresses are added to the Kademlia routing table. New nodes discover the network by connecting to one bootstrap peer.
2. **Manual** — pass `--peers` on startup as fallback:

```bash
SENTRIX_VALIDATOR_KEY=<key> sentrix start --peers [NODE_IP]:30303,[NODE_IP]:30303
```

Peers go through chain_id verification before being added to `verified_peers`. Wrong chain_id = disconnected immediately.

Bootstrap peers re-dialed every 30s if disconnected. Idle timeout set to `Duration::MAX` — connections stay open permanently.

Kademlia protocol: `/sentrix/kad/1.0.0` with in-memory store.

## Block & Transaction Propagation (Gossipsub)

Blocks and transactions propagate via gossipsub pub/sub:

| Topic | Content | Format |
|-------|---------|--------|
| `sentrix/blocks/1` | New blocks | bincode-encoded `GossipBlock` |
| `sentrix/txs/1` | New transactions | bincode-encoded `GossipTransaction` |

Gossipsub config: 5s heartbeat, strict validation, 10 MiB max message size.

When a gossipsub message arrives, the block/transaction is validated via the same `add_block()` / `add_to_mempool()` path as request-response messages.

## Request-Response Messages

Length-prefixed **bincode** over RequestResponse protocol. 4-byte BE length header, 10 MiB cap.

Switched from JSON in v2.0.0 for ~3-5x smaller messages and faster serialization.

| Message | What it does |
|---------|-------------|
| `NewBlock` | Broadcast new block to all peers |
| `NewTransaction` | Propagate new tx |
| `GetBlocks` | Request blocks from a height range |
| `BlocksResponse` | Return up to 50 blocks |
| `Handshake` | Chain ID check on connect |

## Sync

Every 30 seconds, check if any peer is ahead. If yes:

1. Request blocks from `local_height + 1` in batches of 50
2. Validate each block via `add_block()` (same two-pass as locally produced blocks)
3. Save to MDBX immediately after each accepted block
4. Repeat until caught up

Sync and block processing run in `tokio::spawn()` tasks — they don't block the swarm event loop.

## Rate Limiting

- Max 5 connections per IP per 60s
- IPs that exceed this get banned for 5 minutes
- Max 50 total peers
- Rate limiter state auto-pruned periodically

## Ports

| Port | Use |
|------|-----|
| 30303 | P2P (libp2p) |
| 8545 | REST API + JSON-RPC + Explorer |

## Behaviour

```rust
struct SentrixBehaviour {
    identify: identify::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    gossipsub: gossipsub::Behaviour,
    rr: request_response::Behaviour<SentrixCodec>,
}
```

- **Identify**: peer info exchange, feeds addresses into Kademlia
- **Kademlia**: DHT-based peer discovery with periodic bootstrap
- **Gossipsub**: pub/sub block and transaction propagation
- **RequestResponse**: sync, handshake, height queries (bincode codec)
