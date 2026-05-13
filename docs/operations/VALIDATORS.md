# Validators

Sentrix runs **Voyager DPoS + BFT** with a permissionless, uncapped
candidate set and a top-21 active set rotated by total stake. Block
time targets ~2.5 s. Block producer at height `h` = `active_set[h %
active_set.len()]`.

> [!NOTE]
> **To run your own validator:** see **[VALIDATOR_ONBOARDING.md](./VALIDATOR_ONBOARDING.md)** —
> permissionless since 2026-04-25 (Voyager activation), candidate cap
> lifted in v2.2.11 (2026-05-13).

## Current active set (reference fleet)

| Slot | Name | Address prefix | Role |
|------|------|---------------|------|
| 0 | Sentrix Treasury   | `0x0804…` | Treasury validator |
| 1 | Sentrix Foundation | `0x753f…` | Foundation validator |
| 2 | Sentrix Core       | `0x87c9…` | Core validator |
| 3 | Sentrix Beacon     | `0x4cad…` | Beacon validator |

Sorted by address. The reference fleet is operator-run on hosts owned
by Sentrix Labs; this is the seed set, not a permanent ceiling.
**Anyone with ≥15,000 SRX bonded can register and join the active set
once they rank top-21 by stake.**

(Nusantara / BlockForge Asia / PacificStake / Archipelago —
decommissioned during the v2.0.0 reset; services stopped, never on
mainnet's current registry.)

## Active-set rules

| Parameter | Value | Source |
|---|---|---|
| `MIN_SELF_STAKE` | 15,000 SRX | `sentrix-staking/staking.rs` |
| `MAX_ACTIVE_VALIDATORS` | 21 | `sentrix-staking/staking.rs` |
| `MAX_CANDIDATES` | `usize::MAX` (was 100, lifted v2.2.11) | `sentrix-staking/staking.rs` |
| `MIN_BFT_VALIDATORS` | 4 (Voyager activation floor) | `sentrix-staking/staking.rs` |
| `MIN_ACTIVE_VALIDATORS` | 1 (chain can produce with 1 active) | `sentrix-core/authority.rs` |
| Commission range | 0 – 10,000 bp (0 – 100%) | `sentrix-staking/staking.rs` |
| Unbonding period | 201,600 blocks (~7 days at 3 s) | `sentrix-staking/staking.rs` |

## Add a validator (permissionless path, current)

Submit `StakingOp::RegisterValidator` from your wallet — the tx is its
own admission proof. See **[VALIDATOR_ONBOARDING.md §8](./VALIDATOR_ONBOARDING.md#8-register-as-a-validator-permissionless)**
for the full flow.

```text
tx.from_address  = your-wallet (becomes validator address)
tx.to_address    = TOKEN_OP_ADDRESS
tx.amount        = ≥ 15,000 SRX  (bond moved to protocol treasury)
tx.data          = StakingOp::RegisterValidator {
                       self_stake: u64,        // must equal tx.amount
                       commission_rate: u16,   // 0..=10000 bp
                       public_key: String,     // uncompressed secp256k1 hex
                   }
```

No admin co-sign, no whitelist, no Foundation approval.

## Admin path (legacy / emergency)

> [!CAUTION]
> The admin-curated `sentrix validator add` path is **not the normal
> onboarding flow today**. It's retained for:
>
> - Emergency recovery (chain-halted, can't process registration tx)
> - Pre-Voyager bootstrap (no DPoS yet, no stake to bond against)
>
> If you're running a validator under standard ops, use the
> permissionless `StakingOp::RegisterValidator` path above.

```bash
# Admin co-signs (requires the chain admin key)
sentrix validator add --address 0x... --public-key 04... \
  --name "Operator Name" --admin-key <hex>

# Live-node guard (v2.2.11+): if the node is producing blocks during
# the call, the command refuses to persist (avoids overwriting in-
# flight state). Stop the validator first, or set
# SENTRIX_ALLOW_ONLINE_VALIDATOR_MUTATION=1 in deliberate recovery.
```

## Set-change procedure (multi-node coordination)

> [!IMPORTANT]
> Round-robin depends on every node having an identical active set.
> Adding a validator to some nodes but not others = stalled chain.

```
1. Stop ALL validator nodes
2. Run add/remove on EVERY data directory
3. Start ALL nodes simultaneously
```

This is operationally heavy and only relevant to the **admin path**.
The permissionless path applies the change as a normal block tx —
every node sees it via consensus, no coordination needed.

## CLI commands

```bash
sentrix validator list                                 # all candidates + status
sentrix validator unjail --address 0x...               # re-enter after jail
sentrix validator force-unjail --address 0x... \      # phantom-stake unjail (mainnet
    --i-understand-phantom-stake                        # acknowledgement required)
sentrix validator toggle --address 0x... --admin-key  # active <-> inactive (admin)
sentrix validator rename --address 0x... --name "…"   # display name (admin)
sentrix validator remove --address 0x... --admin-key  # full removal (admin)
sentrix validator transfer-admin --new-admin 0x... \  # rotate admin key
    --admin-key <hex>
```

Status shown in `validator list` (v2.2.11+):

- `[JAILED]` — `stake_registry.is_jailed` (downtime / double-sign / unjail-pending)
- `[INACTIVE]` — admin toggled the authority entry off
- `[ACTIVE]` — neither jailed nor toggled

## Block production economics

At ~2.5 s block time and 21 active validators in round-robin, each
active validator produces roughly:

- **3,433 blocks/day** (24 × 3,600 / 2.5 / 21 ≈ 3,432.5)
- **~3,433 SRX/day** in block reward (1 SRX per block, pre-halving)
- Plus `commission_rate × validator_pool_share` of delegated rewards
- Plus a share of tx fees in produced blocks

Delegators get the rest of the validator's pool, distributed proportionally.

## Audit trail

Every admin op logs to the append-only `admin_log` on
`AuthorityManager`:

```bash
curl -H "X-API-Key: <key>" https://rpc.sentrixchain.com/admin/log
```

## See also

- **[VALIDATOR_ONBOARDING.md](./VALIDATOR_ONBOARDING.md)** — full validator setup walkthrough
- **[VALIDATOR_GUIDE.md](./VALIDATOR_GUIDE.md)** — cheat-sheet quickstart
- **[../tokenomics/STAKING.md](../tokenomics/STAKING.md)** — staking + delegation + reward mechanics
- **[CLAIM_REWARDS.md](./CLAIM_REWARDS.md)** — claim accrued rewards (post-V4 fork)
- **[MONITORING.md](./MONITORING.md)** — Prometheus + journald setup
