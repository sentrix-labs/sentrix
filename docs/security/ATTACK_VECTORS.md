# Attack Vector Analysis

13 attack scenarios analyzed across network, consensus, state, and API layers.

---

## Network

### 1.1 DDoS on P2P (port 30303)

Protected. Max 50 peers, 5 conn/IP/60s rate limit, 5-min ban, 10 MiB message cap, Noise XX handshake overhead.

### 1.2 DDoS on RPC (port 8545)

Protected. 60 req/min per IP, 500 concurrent, 1 MiB body limit, CORS restrictive by default. Deploy behind nginx/Cloudflare for extra safety.

### 1.3 Sybil — Fake Nodes

Peer cap limits damage but no reputation system yet. All peers treated equally. No subnet diversity check — one /24 could fill all slots.

Risk: MEDIUM. Peer cap prevents resource exhaustion but attacker could still occupy slots.
Todo: Peer scoring, subnet diversity, authenticated handshake.

### 1.4 Eclipse — Isolate a Validator

Bootstrap peers re-dialed every 30s. Block validation catches bad blocks. But sync picks from one peer (not random), no minimum peer requirement, no checkpoints.

Risk: LOW. Needs control of many IPs + bootstrap knowledge.

### 1.5 MITM

Protected. Noise XX = encrypted + mutually authenticated. PeerId from Ed25519 key.

---

## Consensus

### 2.1 Validator Spoofing

Protected. `add_block()` checks `is_authorized()` per height. Validator registration requires crypto proof (pubkey → address derivation). No block-level signatures yet though — planned for Voyager.

### 2.2 Double Spend

Protected. Strict nonce validation, txid dedup, balance includes pending, chain_id replay protection, canonical BTreeMap signing payload.

### 2.3 Block Withholding

Validator doesn't broadcast → chain stalls at their slot. No timeout/skip mechanism exists.

Risk: MEDIUM. Most likely real-world issue (node goes offline).
Todo: Implement validator timeout + skip to next in rotation.

### 2.4 Long Range Attack

Protected. Only authorized validators can produce. Timestamp bounds enforced. State root in block hash (post 100K). Would need multiple compromised validator keys.

### 2.5 Nothing-at-Stake

N/A — PoA, not PoS. Relevant for Voyager (DPoS). Will need slashing + unbonding period.

---

## State

### 3.1 State Bloat

Min fee is low (0.0001 SRX). Mempool caps (10K total, 100/sender, 1hr TTL) help. But AccountDB grows unbounded — no zero-balance pruning.

Risk: MEDIUM long-term.

### 3.2 Storage Exhaustion

Blocks never deleted (by design). sled grows linearly. At current block size and 3s intervals, this is manageable for years. `db_size_bytes()` available for monitoring.

### 3.3 Trie Manipulation

Protected. BLAKE3 + SHA-256 domain-separated hashing. State root verified on received blocks (post 100K). Content-addressed storage = corruption changes hash immediately.

---

## API

### 4.1 RPC Spam

Protected. Rate limiting, concurrency cap, body limit, batch limit (100), API key on write endpoints.

### 4.2 Malformed JSON-RPC

Protected. Proper error codes (-32700, -32600, -32601). Safe serde deserialization. No panics.

### 4.3 Heavy Queries

`/chain/validate` gated + cached. Listings paginated (max 100). `/richlist` could be slow on large state — should add pagination + auth.

### 4.4 API Key Bypass

Constant-time comparison via `subtle`. If `SENTRIX_API_KEY` not set, everything's open (by design for dev, not for prod).

---

## Risk Matrix

```
                 LOW        MEDIUM      HIGH
           ┌──────────┬──────────┬──────────┐
  HIGH     │          │ Block    │          │
           │          │ withhold │          │
  MEDIUM   │          │ Sybil    │          │
           │          │ Bloat    │          │
  LOW      │ Long rng │ RPC misc │ Malformd │
           │ Dbl spnd │          │          │
           └──────────┴──────────┴──────────┘
```

HIGH+HIGH quadrant is empty. No critical-and-likely attacks.

---

## Priority

P0 (done ✅): libp2p peer limit, per-IP rate limit, legacy TCP deprecated.

P1: Block skip mechanism, peer reputation, randomize sync peer.

P2: Block signatures, richlist pagination, API key startup warning, raise min fee.
