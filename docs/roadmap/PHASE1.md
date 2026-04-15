# Pioneer — PoA Chain (Complete)

Done. Live in production.

## What's Running

- PoA consensus, round-robin, 3s blocks
- 7 validators on 3 VPS, full mesh peering
- Account model with Ethereum-style addresses
- Two-pass atomic block validation
- ECDSA secp256k1 signing with chain_id replay protection
- SentrixTrie (256-level Binary SMT, BLAKE3+SHA-256)
- SRX-20 tokens (SNTX deployed, 10B supply)
- libp2p networking (Noise XX + Yamux)
- REST API (25+ endpoints) + JSON-RPC 2.0 (20 methods)
- Block explorer (12 pages, dark theme)
- Encrypted keystore (AES-256-GCM, Argon2id)
- sled storage with 1000-block sliding window
- CI/CD: GitHub Actions → test → build → 3-VPS deploy
- Branch protection on main (PR + CI required)

## Numbers

| | |
|-|-|
| Tests | 277+ |
| PRs merged | #1–#81 |
| Audit rounds | 11 (94 findings, 78 fixed) |
| Chain height | 131,000+ |
| `unsafe` blocks | 0 |
| Clippy warnings | 0 |

## Security Audits

| Audit | Findings | Status |
|-------|----------|--------|
| V4 | 23 | All fixed ✅ |
| V5 | 11 | All fixed ✅ |
| V6 | 13 | All fixed ✅ |
| V7 | 15 | All fixed ✅ |
| V8 | 12 | All fixed ✅ |
| V9 | 20 | 4 fixed, 16 tracked |
| V10 | Enterprise — 43/45 (95%) | Complete ✅ |
| V11 | 22 | Report available |

## What's Next

Next up: Voyager (DPoS + BFT + EVM). See [PHASE2.md](PHASE2.md).
