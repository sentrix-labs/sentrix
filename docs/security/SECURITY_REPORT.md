# Security Report v1.0

Production-ready, mainnet live.

The codebase enforces zero `unsafe`, a no-panic policy via CI, checked arithmetic, and constant-time crypto on auth-sensitive paths.

Main gap is DoS resistance on the network layer (mostly addressed now).

## Chain

| | |
|-|-|
| Rust 2024, 50+ files, ~22,500+ LoC | 551 tests |
| Voyager DPoS+BFT, 1s blocks (v2.1.39+) — succeeded Pioneer PoA round-robin 2026-04-25 | Chain ID 7119 |
| 315M SRX max supply (post-v2 fork; 210M pre-fork) | 12 crates + 1 binary |

## Code Audit

Full report: [SECURITY_AUDIT_V11.md](SECURITY_AUDIT_V11.md)

The most recent code review surfaced 2 high-severity findings (fee tracking split, timestamp non-determinism — neither is fund-loss), both since fixed, plus a set of medium / low / informational findings tracked in the linked report.

## Attack Vectors

Full report: [ATTACK_VECTORS.md](ATTACK_VECTORS.md)

13 vectors analyzed. HIGH+HIGH quadrant empty. Biggest real risk: block withholding (validator offline → chain stall). P0 network items all fixed.

Already solid: tx signing, double spend protection, mempool caps, rate limiting, state trie proofs, chain_id replay protection, validator crypto verification.

## Pentest

Full report: [PENTEST_RESULTS.md](PENTEST_RESULTS.md)

Scenarios covered: RPC flood, P2P flood, tx spam, malformed input, double spend, oversized payloads. Methodology, raw output, and per-scenario verdicts in the linked report.

## What to Fix

Done ✅: libp2p peer limit, per-IP rate limit, legacy TCP deprecated.

Next: Block skip mechanism, peer reputation, sync randomization, block-level signatures (for Voyager).

## Context

The chain currently runs on 3 VPS with 3 validators under founder control. In this environment, actual risk from all findings is LOW. Risk increases as the chain opens to public validators and external traffic.
