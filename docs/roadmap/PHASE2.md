# Phase 2 — DPoS + BFT + EVM (Planned)

> Design finalized. Not implemented yet. Target: Q3 2026.

## Three Things

### 1. DPoS

Replace admin-appointed validators with stake-weighted selection.

- Min self-stake: 15,000 SRX
- Active set: top 100 by `self_stake + delegated_stake`
- Epoch: 28,800 blocks (~1 day) — validator set recalculated at boundary
- Slashing: 1-5% for downtime, 20% + permaban for double-signing
- Commission: validators set their own rate (5-20%)

### 2. BFT Finality

After a block is produced, validators vote. 2/3+ votes = block is final, can't be reorged. With 100 validators, need 67+ votes.

### 3. EVM via revm

Smart contracts in Solidity. Using [revm](https://github.com/bluealloy/revm) (Paradigm's EVM, used by reth). Battle-tested, pure Rust, audited.

Gas pricing: 0.1 sentri/gas. Block gas limit: 30M. Transfer ≈ 0.000021 SRX.

Why revm and not custom VM: DPoS + BFT is already hard. EVM compatibility gives immediate access to existing tooling. Custom VM can wait for Phase 4 if demand justifies it.

## TODO

- [ ] DPoS: stake registry, delegation, epoch logic, commission, rewards, unbonding, slashing
- [ ] BFT: block signatures, vote collection, 2/3+ threshold, fork choice
- [ ] EVM: revm integration, gas metering, gas limit, eth_call, eth_estimateGas

## Migration

No chain reset. Protocol upgrade at a predetermined fork height. Existing 7 validators grandfathered in. Staking opens for new validators.

## Prerequisites

- [x] P0 security (peer limits, rate limiting)
- [ ] Block-level validator signatures
- [ ] Fork choice rule
- [ ] State root rollback mechanism

## Future Phases

**Phase 3 (Q4 2026, if needed):** Sharding. Only if >1K TPS demand proven.

**Phase 4 (2027+, if demand):** Custom VM with parallel execution. 90% Solidity compat. Would need external audit (Zellic/Trail of Bits).
