# Staking (Phase 2 — Planned)

> Not implemented yet. This is the finalized design.

## How It Works

Phase 2 switches from PoA (admin picks validators) to DPoS (stake picks validators). Anyone with 15K SRX can register. Token holders delegate to their preferred validators.

**Active set:** Top 100 by `self_stake + delegated_stake`. Recalculated every epoch (28,800 blocks ≈ 1 day).

## Delegation

Lock SRX to a validator. You keep ownership, they get voting weight. Rewards split proportionally after validator takes commission (5-20%, self-set).

Claim rewards manually — no auto-compound. Undelegate → unbonding period → SRX returned.

## Slashing

### Liveness (offline)

| Missed blocks | Penalty |
|---------------|---------|
| 1-5 | Warning |
| 6-50 | 1% stake |
| 50+ | 5% stake + kicked |

### Safety (malicious)

| Violation | Penalty |
|-----------|---------|
| Double signing | 20% stake + permanent ban |
| TX manipulation | 20% stake + permanent ban |

Slash split: 50% burned, 50% to Ecosystem Fund. Delegators not directly slashed but should redelegate away from slashed validators.

## Economics

With 100 validators, 3s blocks:
- ~288 blocks/day per validator
- ~288+ SRX/day from rewards (Era 0)
- Plus fee revenue

Cost to attack (51%): 51 validators × 15K SRX = 765K SRX minimum, plus 20% slash risk.

## Gas (EVM)

Phase 2 adds gas metering via revm:

| | |
|-|-|
| Gas price | 0.1 sentri/gas |
| Block gas limit | 30,000,000 |
| Transfer | ~21K gas = 0.000021 SRX |
