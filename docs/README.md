# Sentrix Chain — Docs

## Architecture

- [Overview](architecture/OVERVIEW.md) — components, module map, data flow
- [Consensus](architecture/CONSENSUS.md) — PoA round-robin, block production
- [Networking](architecture/NETWORKING.md) — libp2p, peer management, sync
- [State](architecture/STATE.md) — trie, sled, state roots, merkle proofs
- [Transactions](architecture/TRANSACTIONS.md) — tx lifecycle, fees, nonce, mempool

## Security

- [Code Audit V11](security/SECURITY_AUDIT_V11.md) — source review findings (8.3/10)
- [Attack Vectors](security/ATTACK_VECTORS.md) — 13 scenarios, risk matrix
- [Pentest Results](security/PENTEST_RESULTS.md) — 6/6 passed
- [Security Report](security/SECURITY_REPORT.md) — full report

## Operations

- [Deployment](operations/DEPLOYMENT.md) — build, configure, run a node
- [CI/CD](operations/CI_CD.md) — pipeline, deploy phases
- [Validators](operations/VALIDATORS.md) — setup, registration, current set
- [Monitoring](operations/MONITORING.md) — health checks, troubleshooting

## Tokenomics

- [SRX](tokenomics/SRX.md) — supply, halving, fees
- [Staking](tokenomics/STAKING.md) — DPoS design (Voyager, planned)
- [Token Standards](tokenomics/TOKEN_STANDARDS.md) — SRX-20, SNTX

## Roadmap

- [Pioneer](roadmap/PHASE1.md) — PoA (done)
- [Voyager](roadmap/PHASE2.md) — DPoS + BFT + EVM (planned)
- [Changelog](roadmap/CHANGELOG.md) — PR history

## Quick Ref

| | |
|-|-|
| Chain ID | 7119 |
| Block time | 3s |
| Coin | SRX (1 SRX = 100M sentri) |
| Max supply | 210M SRX |
| Reward | 1 SRX/block, halving every 42M blocks |
| Fees | 50% burn / 50% validator |
| Consensus | PoA (Pioneer) → DPoS (Voyager) |
| License | BUSL-1.1 |

## Quick Start

```bash
cargo build --release
cargo test
sentrix wallet generate
sentrix init --admin-address 0x<addr>
sentrix start --validator-key <key> --peers [PEER]:30303
```
