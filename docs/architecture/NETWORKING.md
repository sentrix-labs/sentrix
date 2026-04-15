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

Manual for now — pass `--peers` on startup:

```bash
sentrix start --validator-key <key> --peers [NODE_IP]:30303,[NODE_IP]:30303
```

Peers go through chain_id verification before being added to `verified_peers`. Wrong chain_id = disconnected immediately.

Bootstrap peers re-dialed every 30s if disconnected. Idle timeout set to `Duration::MAX` — connections stay open permanently.

## Messages

Length-prefixed JSON over RequestResponse protocol. 4-byte BE length header, 10 MiB cap.

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
3. Save to sled immediately after each accepted block
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
    request_response: request_response::Behaviour<SentrixCodec>,
}
```

Identify for peer info exchange, RequestResponse for everything else.
