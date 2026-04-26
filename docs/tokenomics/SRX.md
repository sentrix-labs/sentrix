# SRX

Native coin. Used for fees, validator rewards, and staking (Voyager).

## Units

```
1 SRX = 100,000,000 sentri
```

Everything internal is u64 in sentri. No floats.

## Supply

Hard cap (post tokenomics-v2 fork): **315,000,000 SRX**. Can't change without another fork.

| Source | SRX | % |
|--------|-----|---|
| Premine | 63,000,000 | 20% |
| Block rewards | ≤252,000,000 | 80% |

### Premine

| Wallet | SRX | Purpose |
|--------|-----|---------|
| Founder | 21,000,000 | Treasury, ops |
| Ecosystem | 21,000,000 | Grants, partnerships |
| Early Validator | 10,500,000 | Validator incentives |
| Reserve | 10,500,000 | Emergency |

### Tokenomics v2 fork (2026-04-26)

The original v1 schedule (1 SRX × 42M halving × 210M cap) had a math gap: geometric series asymptoted at 84M from mining + 63M premine = 147M effective max, leaving 63M of the 210M cap unreachable. The v2 fork retargets emission to BTC-parity 4-year halving (126M blocks at 1s) + raises cap to 315M. Geometric: 1 SRX × 126M × 2 = 252M from mining + 63M premine = 315M (reachable). Premine ratio drops 30% nominal v1 → 20% v2 (industry-leading optics). Validator runway extended to ~year 20.

Activated:
- Testnet: h=381651 (2026-04-26 afternoon)
- Mainnet: h=640800 (2026-04-26 evening, env-armed)

## Halving (post v2 fork)

Block reward halves every 126M blocks (~4 years, BTC parity at 1s blocks).

| Era | Blocks (post-fork-relative) | Reward | Start (~) |
|-----|--------|--------|-------|
| 0 | 0 – 125.9M | 1 SRX | 2026 |
| 1 | 126M – 251.9M | 0.5 SRX | ~2030 |
| 2 | 252M – 377.9M | 0.25 SRX | ~2034 |
| 3 | 378M – 503.9M | 0.125 SRX | ~2038 |

Pre-fork blocks (history before fork-height was reached) used the v1 42M-block schedule. Halving count is fork-aware: pre-fork uses `height / 42M`, post-fork uses `(height - fork_height) / 126M` so cumulative halvings don't reset at fork moment.

```rust
fn get_block_reward(height: u64) -> u64 {
    let halvings = halvings_at(height); // fork-aware
    BLOCK_REWARD.checked_shr(halvings).unwrap_or(0)
        .min(max_supply_for(height) - total_minted)
}
```

## Fees

Min fee: 0.0001 SRX (10,000 sentri).

```
ceil(fee/2) → burned permanently
floor(fee/2) → block validator
```

Odd sentri goes to burn. Eventually burns > rewards = deflation.

## Address Format

`0x` + 40 hex chars. Derived from `Keccak-256(secp256k1 pubkey)[12..32]`. Same as Ethereum — MetaMask/ethers.js work out of the box.

## Constants

```rust
CHAIN_ID                = 7119
MAX_SUPPLY              = 21_000_000_000_000_000  // 210M in sentri (v1, pre-fork)
MAX_SUPPLY_V2           = 31_500_000_000_000_000  // 315M in sentri (v2, post-fork)
BLOCK_REWARD            = 100_000_000             // 1 SRX (unchanged across forks)
HALVING_INTERVAL        = 42_000_000              // blocks (v1)
HALVING_INTERVAL_V2     = 126_000_000             // blocks (v2, BTC-parity 4y at 1s)
BLOCK_TIME_SECS         = 1
MIN_TX_FEE              = 10_000                  // 0.0001 SRX
CHAIN_WINDOW_SIZE       = 1000
STATE_ROOT_FORK_HEIGHT  = 100_000
TOKENOMICS_V2_HEIGHT    = u64::MAX                // env-overridable; default disabled
```
