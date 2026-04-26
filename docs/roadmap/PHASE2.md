# Voyager — DPoS + BFT + EVM (LIVE on mainnet)

> **Status: ACTIVE on mainnet since 2026-04-25 (h=579047).** Both networks (mainnet chain_id 7119, testnet chain_id 7120) run Voyager DPoS+BFT with EVM (revm 37) enabled.

Voyager succeeded Pioneer (PoA round-robin, blocks 0…579046 on mainnet) as the consensus + execution engine. The transition was a hard-fork at h=579047 — `voyager_activated=true` flag set on chain.db, all 4 mainnet validators migrated together via parallel restart with the L2 cold-start gate ensuring mesh-stable BFT entry.

## Three Pillars (live)

### 1. DPoS Validator Selection

Stake-weighted active set, replacing the admin-appointed Pioneer authority.

- **Self-stake minimum:** 15,000 SRX (`MIN_SELF_STAKE`)
- **Active set:** top 100 validators by `self_stake + delegated_stake × commission_factor`. Currently 4 active on mainnet (Foundation, Treasury, Core, Beacon).
- **Epoch:** 28,800 blocks (~1 day at 1s block time) — validator set recalculated at boundary; jailing/slashing committed.
- **Slashing:**
  - Downtime: 1-5% gradual, scales with offline duration
  - Double-signing: 20% + permanent ban (tombstoned)
  - Submitted via `StakingOp::SubmitEvidence` from any active validator
- **Commission:** each validator sets their own rate (current default 10%, range 5-20%)

### 2. BFT Finality

After a proposer drafts a block, all active validators vote in two phases (Tendermint-style):

- **Round 0:** Propose → Prevote → Precommit. If 2/3+1 stake-weighted precommits land in the round window, block finalizes.
- **Skip rounds:** if quorum not reached, round advances; proposer rotates per `(height + round) % active_set.len()`. Locked-block re-propose preserves PoLC integrity across rounds.
- **Justifications:** each finalized block carries a `BlockJustification` with the precommit signatures that finalized it. Light clients verify finality by checking justifications against the on-chain stake registry.

For a 4-validator mesh, supermajority threshold = 3 of 4 (75%). Mainnet routinely finalizes round-0 at 1 block/sec under nominal load.

### 3. EVM via revm 37

Solidity smart contracts via [revm](https://github.com/bluealloy/revm) (Paradigm's EVM, used by Reth/Erigon). Pure Rust, battle-tested, currently on revm 37.

- **Activation:** `evm_activated=true` set 2026-04-25 in the same window as Voyager activation (h=579060).
- **Gas pricing:** 0.1 sentri/gas; block gas limit 30M; basic transfer ≈ 0.000021 SRX.
- **MetaMask compatibility:** `eth_sendRawTransaction`, `eth_call`, `eth_getBalance`, `eth_estimateGas`, `eth_getCode`, `eth_getStorageAt` all supported. Chain ID 7119 mainnet, 7120 testnet.
- **Tx encoding:** `data` field starting with `EVM:` routes through revm; otherwise standard Sentrix tx.

## V4 Reward Distribution (active subsystem)

Layered onto Voyager since h=590100 (2026-04-25). Pre-V4 the proposing validator was credited 1 SRX direct; post-V4 the reward routes to `PROTOCOL_TREASURY` (`0x0000000000000000000000000000000000000002`) escrow.

```
block produced → coinbase 1 SRX → PROTOCOL_TREASURY (escrow)
                                   ↓
            ClaimRewards staking op ← validator/delegator
                                   ↓ (apply-time)
                  TREASURY → claimer balance
                  pending_rewards reset to 0
```

Why:
- Stake-weighted delegator share (delegators earn pro-rata from validator's commission carve-out without manual accounting)
- Slashing applies to `pending_rewards` before claim — misbehavior reduces accumulated reward, not yet-paid balance
- Audit trail visible via `/staking/validators` JSON-RPC

See [docs/operations/CLAIM_REWARDS.md](../operations/CLAIM_REWARDS.md) for the operator guide.

## Network Hardening

The 2026-04-25 / 2026-04-26 marathon shipped substantial network hardening alongside Voyager activation:

- **L1 multiaddr advertisements** (v2.1.26+): each validator broadcasts a signed `MultiaddrAdvertisement` on the `sentrix/validator-adverts/1` gossipsub topic at startup + every 10 minutes; receivers cache + dial. Self-healing mesh from a single bootstrap peer.
- **L2 cold-start gate** (v2.1.27): validator loop refuses to enter BFT mode unless `peer_count >= active_set.len() - 1`. Closes the cold-start race that caused the 2026-04-25 activation #1 livelock.
- **Connection-leak fixes** (v2.1.31-v2.1.34): dial-tick connected-peers pre-check + `/p2p/<peer_id>` in advert multiaddrs + `connection_limits::Behaviour` cap (max 2 established per peer). Closes the connection-accumulation pattern.
- **Runtime-aware Voyager dispatch** (v2.1.33): `voyager_mode_for(&self, height)` ORs env-var fork-height check with chain.db `voyager_activated` runtime flag. Closes the env-var-default-`u64::MAX` foot-gun that caused validate_block to fall into Pioneer auth.

## Operations

- **Mainnet:** 4 validators (Foundation, Treasury, Core, Beacon). Each lists all 3 others in systemd `--peers` for clean cold-start convergence.
- **Testnet:** 4 validators in Docker on the build host. Same Voyager+EVM+V4 stack as mainnet; chain_id 7120; height ~200K+.
- **Binary:** v2.1.36 across both networks.
- **RPC reporting:** `/sentrix_status` returns `consensus: "DPoS+BFT"`; `/chain/info` exposes `consensus_mode`, `voyager_activated`, `evm_activated`; `/staking/validators` returns per-validator `pending_rewards`.

## Outstanding Voyager work (defence-in-depth, not blocking)

- **BFT signing v2** (chain_id in signing payload + low-S enforcement) — hard-fork-gated. Phase 1 foundation shipped, Phase 2 call-site refactor pending dedicated session. Defence-in-depth — closes cross-chain BFT vote replay vulnerability before external validator onboarding.
- **External validator onboarding tooling** — DPoS open registration is live (15,000 SRX self-stake floor). Operator runbook + automation polish before opening to third parties.
- **ClaimRewards CLI** (alongside the existing `tools/claim-rewards/` standalone binary) — direct `sentrix validator claim-rewards` subcommand for ergonomic operator UX.

## What's Next: Frontier

See [PHASE3.md](./PHASE3.md). Frontier targets parallel transaction execution + sub-1s block time + ecosystem expansion via mainnet hard fork. Phase F-1 type scaffold + F-2 shadow-mode wiring already in main; F-3 onward (real parallel apply) ~6-8 weeks calendar.
