# Sentrix EIP-1559 — Dynamic Base Fee Spec

Sentrix is moving from a flat `INITIAL_BASE_FEE` to EIP-1559-style
dynamic base fee, matching Ethereum's fee-pricing model. This doc
specifies the mechanic, user-facing tx shape, and what it means for
wallets / indexers / dApps.

> Scope: behavior spec only. Implementation planning (rollout
> sequencing, test strategy, migration risks) lives in the internal
> companion doc.

---

## 1. Motivation

Today every Sentrix transaction pays a fixed fee regardless of
network load. That has two problems:

1. **Fee discovery is manual.** Wallets can't auto-compute a
   reasonable fee — they either over-pay or risk a queued tx if the
   mempool spikes. MetaMask's "Market / Aggressive / Low" picker
   relies on having a `baseFeePerGas` to compute against; without
   it the wallet shows legacy fee fields that users don't understand.
2. **No anti-spam pressure.** Under a sudden load (NFT mint, airdrop
   claim, DEX incident), the mempool fills up and honest users can't
   out-bid the spam because everyone pays the same flat fee. EIP-1559
   raises the base fee as blocks fill, pricing out spam algorithmically.

EIP-1559 also adds a deflationary pressure: the base fee is **burned**,
reducing circulating supply under high load. Sentrix already has a
50% fee burn (`block_executor.rs:611-617`); this formalises it as the
base-fee mechanic.

---

## 2. Mechanic

### 2.1 Base fee update rule

Per-block:
```
base_fee_next = base_fee_current × (1 + (gas_used - gas_target) / gas_target / 8)
              capped to [base_fee_current × 7/8, base_fee_current × 9/8]
```

- **Gas target** = 50% of block gas limit (= `BLOCK_GAS_LIMIT / 2`)
- **Max adjustment per block** = ±12.5%
- **Floor** = 1 sentri (never goes to zero)

Identical to Ethereum's EIP-1559 formula (for wallet compatibility).

### 2.2 Genesis base fee

First post-fork block: `INITIAL_BASE_FEE` constant (same value as
current flat fee, so there's no user-visible price shock at fork
moment).

### 2.3 Transaction fee model

Two new fields on EVM transactions (already standard in Ethereum
ecosystem):

| Field | Meaning |
|---|---|
| `max_fee_per_gas` | Absolute ceiling the user is willing to pay per gas |
| `max_priority_fee_per_gas` | Tip on top of base_fee that goes to the validator |

Per-tx calculation on inclusion:
```
effective_gas_price = min(max_fee_per_gas, base_fee + max_priority_fee_per_gas)
tx_fee = effective_gas_price × gas_used
burn   = base_fee × gas_used
tip    = tx_fee - burn
```

**Tip goes to the proposer's pending rewards** (via
`distribute_reward`). **Burn is deducted from circulating supply**.

### 2.4 Native Sentrix (non-EVM) transactions

Legacy Sentrix tx shape (SRX transfer, SRC-20 ops, staking) continues
to have a flat `fee` field for now. Rationale: native ops don't use
gas metering — the fee is a simple flat anti-spam charge. Keeping
them separate from EIP-1559 avoids forcing wallets to know two fee
models for the same chain.

A future release may bring native ops under a flat-but-dynamic fee
adjusted per block (similar base-fee mechanic but without `gas_used`
variability, using tx count instead). Out of scope for this spec.

### 2.5 EVM transaction encoding

Sentrix EVM txs are encoded as `EVM:gas_limit:calldata_hex` in the
legacy Sentrix `data` field, with the original RLP-encoded Ethereum
tx living in the `signature` field (see `crates/sentrix-evm/src/lib.rs`).

After EIP-1559 activation:
- **EIP-1559 txs (type 2)**: `max_fee_per_gas` + `max_priority_fee_per_gas`
  are RLP-encoded in the signature field per standard. Sentrix
  decodes them during `execute_evm_tx_in_block`.
- **Legacy txs (type 0)**: still accepted. Treated as EIP-1559 with
  `max_fee_per_gas = max_priority_fee_per_gas = gas_price`. This is
  what Geth does.
- **EIP-2930 txs (type 1)**: accepted, same treatment as legacy.

---

## 3. RPC surface

### 3.1 `eth_getBlockByNumber` / `eth_getBlockByHash`

Every returned block gains two new fields:
```json
{
  "baseFeePerGas": "0x3b9aca00",     // base_fee at this block
  ...
}
```

### 3.2 `eth_feeHistory`

New RPC method that returns recent base_fee history + percentile
reward samples. Required for wallet fee estimation.

```json
{
  "jsonrpc":"2.0","method":"eth_feeHistory",
  "params":["0xa", "latest", [25, 50, 75]]
}
```

Returns:
```json
{
  "oldestBlock": "0x12a34",
  "baseFeePerGas": ["0x3b9...", "0x3c0...", ...],   // baseFee for each block, +1 extrapolated
  "gasUsedRatio": [0.43, 0.57, 0.81, ...],
  "reward": [                                         // percentile tips per block
    ["0x4b40", "0x9ca0", "0x13880"],
    ...
  ]
}
```

### 3.3 `eth_gasPrice`

Returns the current `base_fee + 1 gwei` as a sensible default for
legacy-type callers. Not authoritative — EIP-1559 wallets should use
`eth_feeHistory` instead.

### 3.4 `eth_maxPriorityFeePerGas`

Returns a suggested priority tip based on recent blocks (typically
~1 gwei, bumps under load). Used by wallets to default the tip slider.

### 3.5 `eth_getTransactionReceipt`

Already returns `effectiveGasPrice`; no change. Now derived from
base_fee at the receipt's block instead of being a constant.

### 3.6 `sentrix_baseFee`

New Sentrix-native method for simple base-fee lookup without
EIP-1559 dev-tooling:
```json
{"jsonrpc":"2.0","method":"sentrix_baseFee","params":[]}
→ "0x3b9aca00"
```

---

## 4. Migration — what breaks, what doesn't

### 4.1 Mainnet hard fork

Base-fee activation **requires a hard fork** — block.base_fee is a
new consensus-critical field affecting tx fee computation. The
`VOYAGER_EVM_HEIGHT` env var (already used to gate EVM activation)
is extended to also gate EIP-1559.

- **Before fork height**: flat `INITIAL_BASE_FEE`, no base_fee field
  in blocks.
- **At/after fork height**: EIP-1559 active.

A node running old binary past the fork height would reject the new
base_fee field and fork the chain. Coordinate carefully.

### 4.2 Wallet compatibility matrix

| Wallet | Legacy tx | EIP-1559 tx | Notes |
|---|---|---|---|
| MetaMask | ✅ (auto-downgrade) | ✅ (default when detected) | Native support; uses `eth_feeHistory` |
| Rabby | ✅ | ✅ | Same |
| Rainbow | ✅ | ✅ | Same |
| WalletConnect | transparent | transparent | Whatever the wallet speaks |
| ethers.js v6 | ✅ | ✅ default | Automatic; uses provider's feeHistory |
| viem | ✅ | ✅ default | Automatic |
| web3.js v4 | ✅ | ✅ | Some dApps still on v1; v1 requires explicit config |

All major Ethereum wallets + libraries are EIP-1559 compatible
out-of-box. Legacy transactions keep working.

### 4.3 dApp code changes

Developers don't need to change anything — ethers / viem /
web3.js v4 handle fee fields automatically. Contracts don't see
base_fee at all (it's a block-level concern).

The one change required: dApps that previously hard-coded a fee
value should move to `provider.estimateFeesPerGas()` or equivalent.
This is already common practice.

### 4.4 Explorer (Sentrix Scan)

Block pages need two new fields:
- `baseFeePerGas` — shown prominently
- `gasUsedRatio` — helps user understand why base_fee moved

Tx pages need to distinguish:
- `maxFeePerGas` (set by user)
- `maxPriorityFeePerGas` (set by user)
- `effectiveGasPrice` (actual, computed)
- `baseFeeBurned` (base_fee × gas_used, separate line)
- `tipPaid` (to validator)

### 4.5 Indexer / subgraph

Receipt data has always exposed `effectiveGasPrice`; indexers that
use it continue to work. Indexers that summed fees as `gasPrice ×
gasUsed` get slightly different numbers post-fork, because
`effectiveGasPrice` now tracks base_fee. Not a breaking change —
same interface, different numerical outputs.

---

## 5. Anti-abuse concerns

### 5.1 Oscillation

Max ±12.5% per block means a 10x price movement needs ~20 blocks.
Prevents single-block shocks.

### 5.2 Very low base_fee floor

Floor of 1 sentri means attack-via-price-collapse (fill blocks to
push fee to ~0 so validators earn nothing) costs the same as just
spamming with paid txs. No free lunch.

### 5.3 Validator tip capture

Non-proposer signers under the reward-distribution-v2 proposal (PR
pending) already share reward; tip is part of that pool, split same
way. No special-case handling needed.

---

## 6. Out of scope for this spec

- Fee adjustment for **native (non-EVM) ops**. Keep flat fee in v1.
- Backdated base_fee for blocks before fork height (they don't have
  one; explorers must handle `baseFeePerGas = null` for pre-fork
  blocks gracefully).
- Blob transactions (EIP-4844). Separate future spec.
- Account abstraction (EIP-4337). Separate future spec.

---

## 7. Readiness checklist for activation

Before setting `VOYAGER_EIP1559_HEIGHT` on mainnet:

- [ ] Implementation merged + deployed to testnet, 7+ days of bake
- [ ] `eth_feeHistory` works against at least 2 wallets end-to-end
- [ ] Explorer displays baseFee + gasUsedRatio on block page
- [ ] Wallet-web + mobile wallet updated to use EIP-1559 by default
- [ ] A rollback plan documented (stop-the-world fork at fallback
      height if EIP-1559 misbehaves in production)

Ship as **v2.3 or later**; not bundled with Voyager-fork day.
