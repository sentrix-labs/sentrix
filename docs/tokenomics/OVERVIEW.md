---
sidebar_position: 1
title: Tokenomics Overview
---

# Tokenomics

The full breakdown of SRX supply, distribution, halving schedule, vesting, and reward economics. For the visual version, see [`sentrixchain.com/docs/tokenomics`](https://sentrixchain.com/docs/tokenomics).

## At a glance

| Field | Value | Status |
|---|---|---|
| Hard cap | **315,000,000 SRX** | ✅ Enforced on-chain (3-layer guard) |
| Premine | **63,000,000 SRX** (20%) | ✅ Genesis allocations, immutable |
| Mining supply | **252,000,000 SRX** (80%) | ✅ Geometric series, 4-year halving |
| Block reward (era 0) | **1 SRX/block** | ✅ Live |
| Block time | **1 second** | `BLOCK_TIME_SECS = 1` |
| Halving | every **126,000,000 blocks** (~4 years, BTC-parity) | Active since `TOKENOMICS_V2_HEIGHT=640800` |
| Mining duration | ~108–112 years (asymptotic) | Halving 27 → reward = 0 |
| Native fee burn | **50% burned, 50% to validator** | `account.rs::div_ceil(2)` |
| Decimals | **8** (1 SRX = 100,000,000 sentri) | BTC-style, NOT 18 like ERC-20 |

Live verification:

```bash
curl -s https://rpc.sentrixchain.com/chain/info | jq '{height, max_supply_srx, next_block_reward_srx, total_minted_srx, total_burned_srx}'
```

## 1. Hard cap: 315M SRX

Enforced at three independent layers in `crates/sentrix-core/src/blockchain.rs`:

1. **Constant** `MAX_SUPPLY_V2 = 315_000_000 × 100_000_000` sentri (line 31)
2. **Reward clamp** `reward.min(remaining)` in `Blockchain::get_block_reward` (line 1110)
3. **Mint counter** `total_minted = total_minted.saturating_add(coinbase_amount)` with cap re-check (`block_executor.rs:809`)

The cap is technically asymptotic: integer truncation in the halving series leaves a residue of ~1.88 SRX never minted, so live `total_minted` saturates just below 315M.

## 2. Premine: 63M SRX (20%)

Four genesis wallets, set in [`genesis/mainnet.toml`](https://github.com/sentrix-labs/sentrix/blob/main/genesis/mainnet.toml) and immutable. Sub-allocation policies below are governance intent; each top-level address is publicly auditable on the [explorer](https://scan.sentrixchain.com).

| Slot | Address | Amount | Policy |
|---|---|---|---|
| Founder | `0x5b5b06688dcdbe532353ac610aaff41af825279d` | 21,000,000 SRX | 12-mo cliff + 48-mo linear vesting (60 mo total) — social commitment, on-chain vesting contract pending Q3 2026 |
| Ecosystem Fund | `0xeb70fdefd00fdb768dec06c478f450c351499f14` | 21,000,000 SRX | Multi-sig governed: Dev Grants 8M · Hackathon 3M · Marketing 5M · Faucet 1M · Reserve 4M |
| Validator Incentive Pool | `0x328d56b8174697ef6c9e40e19b7663797e16fa47` | 10,500,000 SRX | Distributed over ~24 months post external-validator onboarding |
| Strategic Reserve | `0x2578cad17e3e56c2970a5b5eab45952439f5ba97` | 10,500,000 SRX | Multi-sig governed: Airdrops 5M · CEX Listings 3M · DEX Liquidity 1.5M · Emergency 1M |

> **Genesis note:** `genesis/mainnet.toml` lists the historical Founder v2 address as the seed (`0x252f8cfed5...`). The live admin Founder v3 (`0x5b5b06688dcd...`) inherited the balance via on-chain admin transfer at block 444070 (txid `0x07354cc4...`, 2026-04-24). Genesis is immutable; the live state on the explorer is correct.

## 3. Halving schedule — BTC-parity

Geometric halving series: each era is **126,000,000 blocks ≈ 4 years**. Initial reward 1 SRX/block.

| Era | Block range (post-fork from h=640,800) | Approx years from launch | Reward (SRX/block) | Era mint (SRX) |
|---|---|---|---|---|
| 0 | 640,800 → 126,640,800 | 0 → ~4 | 1.0 | 126,000,000 |
| 1 | 126,640,800 → 252,640,800 | ~4 → ~8 | 0.5 | 63,000,000 |
| 2 | … | ~8 → ~12 | 0.25 | 31,500,000 |
| 3 | … | ~12 → ~16 | 0.125 | 15,750,000 |
| 4 | … | ~16 → ~20 | 0.0625 | 7,875,000 |
| 5 | … | ~20 → ~24 | 0.03125 | 3,937,500 |
| … | … | … | ÷2 each era | … |
| 26 | … | ~108 | 1 sentri | 1.26 SRX |
| 27+ | from ~year 108 | ~112+ | 0 (integer truncation) | 0 |

**Geometric total:** `1 SRX × 126M × Σ(2⁻ᵏ) = 252M SRX`. Integer-truncation actual: ~252M − 1.88 SRX residue.

Pre-fork (h \< 640,800) used 42M-block halving with a 210M cap. Fork was activated cleanly while still in v1 era 0 (cumulative halvings transition smoothly with no reward jump).

## 4. Burn mechanism

### Native flat-fee (current)

- Min fee: **10,000 sentri = 0.0001 SRX** (`MIN_TX_FEE`, [`crates/sentrix-primitives/src/transaction.rs:9`](https://github.com/sentrix-labs/sentrix/blob/main/crates/sentrix-primitives/src/transaction.rs#L9))
- Split: **50% burned, 50% to block validator** — `burn = fee.div_ceil(2)`, `validator = fee - burn`
- Algebraic invariant: `burn + validator == fee` proved in `account.rs::test_validator_share_lossless`

### Reward routing post-V4-fork

Since `VOYAGER_REWARD_V2_HEIGHT=590100` (2026-04-25), block reward (1 SRX) routes to PROTOCOL_TREASURY escrow instead of directly to the proposer's balance. Validators + delegators drain via `StakingOp::ClaimRewards`. See [Reward Escrow](./REWARD_ESCROW) for the sentinel address explainer.

Fee revenue (post split) still credits the validator immediately every block — only the 1 SRX block reward is escrowed.

### Live (2026-04-28)

Cumulative burned: `~15 SRX` at h≈780k. Burn rate scales with EVM dApp adoption.

## 5. Vesting

| Recipient | Amount | Schedule | On-chain enforcement |
|---|---|---|---|
| Founder | 21,000,000 | 12-mo cliff + 48-mo linear (60 mo total) | ❌ Pending — vesting contract Q3 2026 |
| Ecosystem Fund | 21,000,000 | Multi-sig governance, transparent disbursement | ✅ SentrixSafe |
| Validator Incentive | 10,500,000 | Distributed over 24 months post external-validator launch | ❌ Policy (governance-gated drips) |
| Strategic Reserve | 10,500,000 | Multi-sig governance | ✅ SentrixSafe |

All four wallets publicly auditable on the [explorer](https://scan.sentrixchain.com). Any unscheduled outflow is observable in real-time.

## 6. Airdrop strategy — 5M SRX

Phased rollout from the Strategic Reserve. Each phase has its own snapshot + criteria; distribution via Merkle-claim:

| Phase | Allocation | Audience | Trigger |
|---|---|---|---|
| 1 — Testnet Heroes | 1,000,000 | Active testnet validators + power users | Q2 2026 |
| 2 — Quest Campaign | 1,000,000 | Galxe/Zealy-style task completion | Q3 2026 |
| 3 — Activity Rewards | 800,000 | Active mainnet wallets | Q3 2026 |
| 4 — Validator Delegators | 700,000 | Pro-rata to delegators | Q4 2026 |
| 5 — Retroactive Builders | 1,500,000 | dApp deployers, audit contributors, ecosystem PRs | Q4 2026 / Q1 2027 |

## 7. Listing roadmap

Target tiers, subject to maintainer approval (aggregators) or commercial agreement (CEXs).

| Tier | When | Targets |
|---|---|---|
| 0 — Aggregators (free) | Q2-Q3 2026 | CoinGecko, CoinMarketCap, DefiLlama, Chainlist (PR #8266 live) |
| 1 — Indonesian CEX | Q3-Q4 2026 | Tokocrypto, Pintu, Indodax |
| 2 — International | Q4 2026 - Q1 2027 | Gate.io, MEXC, KuCoin |
| 3 — Top tier | 2027+ | Binance, Coinbase (traction-gated) |

Listing fee budget: 3M SRX from Strategic Reserve.

## 8. Governance

| Period | Setup | Authority signer(s) |
|---|---|---|
| **Today (2026-04-28)** | SentrixSafe 1-of-1 on both chains | Authority `0xa25236925bc10954e0519731cc7ba97f4bb5714b` |
| **Future (no committed timing)** | SentrixSafe N-of-M multi-sig — expansion to multiple signers when independent co-signers are recruited and onboarded | Authority + future co-signers |

Migration history (2026-04-28): `addOwner(authority)` testnet block 881639 + mainnet block 755821; `removeOwner(deployer)` testnet block 884599 + mainnet block 757829. Bootstrap deployer EOA retired from Safe ownership. Tx hashes in [canonical-contracts ADDRESSES.md](https://github.com/sentrix-labs/canonical-contracts/blob/main/docs/ADDRESSES.md).

## 9. Transparency commitments

- **Public tokenomics page:** [sentrixchain.com/docs/tokenomics](https://sentrixchain.com/docs/tokenomics)
- **Real-time wallet labels** in the explorer (Founder, Ecosystem Fund, Reserve, Authority, Safes, Treasury sentinel)
- **On-chain verifiable** — every claim above checks out via `cast call` / RPC against the live chain.

## See also

- [SRX](./SRX) — native coin units, supply, distribution
- [Staking](./STAKING) — Voyager DPoS + reward distribution
- [Token Standards](./TOKEN_STANDARDS) — SRC-20 native + ERC-20 via EVM
- [Reward Escrow](./REWARD_ESCROW) — sentinel addresses + PROTOCOL_TREASURY explainer
