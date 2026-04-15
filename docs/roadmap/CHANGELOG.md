# Changelog

## [0.1.0] — 2026-04-15

Phase 1 complete.

### Added

Core: PoA round-robin consensus, account model, two-pass atomic block validation, ECDSA secp256k1 signing with chain_id, SHA-256 merkle tree, halving (42M blocks), fee split (50% burn / 50% validator), genesis premine (63M SRX), checked arithmetic everywhere.

Trie: 256-level Binary SMT (BLAKE3 leaf + SHA-256 internal), sled-backed (4 trees), LRU cache, merkle proofs, state root in block hash (post height 100K), committed root protection, GC.

Tokens: SRX-20 standard — deploy, transfer, burn, mint, approve/transferFrom. Deterministic contract addresses. Max supply enforcement. SNTX deployed (10B).

Network: libp2p (TCP + Noise XX + Yamux). Persistent Ed25519 identity. Auto-reconnect. Per-IP rate limiting (5/60s, 5-min ban). Max 50 peers. Incremental sync with sled persistence. Block processing in spawned tasks.

API: 25+ REST endpoints. 20 JSON-RPC methods (Ethereum-compatible). 12-page block explorer. Rate limiting (60/min/IP). Constant-time API key comparison. CORS restrictive. 500 concurrency, 1 MiB body, 100 batch.

Wallet: secp256k1 keygen, Keccak-256 addresses. AES-256-GCM keystore. Argon2id v2 KDF (backward-compat PBKDF2 v1). Zeroize on drop.

Storage: sled embedded DB. Per-block persistence + hash index. 1000-block sliding window.

Infra: 17 CLI commands. CI/CD (cargo deny → clippy → build → test → 3-VPS deploy). 4-phase deploy with health check. Branch protection.

Security: 11 audit rounds (94 findings, 78 fixed). Zero `unsafe`. No-panic CI enforcement. 6/6 pentest pass.

### Major PRs

| PR | What |
|----|------|
| #36–#40 | Security V4 (23 findings fixed) |
| #41 | Security V5 (11 findings) |
| #43 | Split blockchain.rs → 6 modules |
| #44 | Security V6 (13 findings) |
| #45 | libp2p integration |
| #46 | Integration tests (45 tests) |
| #48–#55 | SentrixTrie |
| #57–#60 | Security V7 |
| #65 | libp2p default (legacy TCP removed) |
| #69 | Idle timeout fix |
| #72–#73 | Security V8+V9 |
| #74 | Public repo cleanup |
| #79 | H1/H2 fork fix |
| #80 | CI/CD deploy order fix |
| #81 | VPS3 + 3-VPS pipeline |
| #82 | P0 security hardening |
