# Sentrix Airdrop Mechanics

**Last updated:** 2026-04-28
**Total allocation:** 5,000,000 SRX (10% of premine, ~1.6% of max supply 315M)
**Source wallet:** Strategic Reserve `0x2578cad17e3e56c2970a5b5eab45952439f5ba97`
**Status:** Phase 1 design-phase. No phase has launched yet.

## Why this exists

Sentrix Chain's positioning is "financial infrastructure for the real economy." That posture shapes airdrop design: rewards must tie to **real participation** (testnet usage, dApp building, validator delegation, ecosystem contribution), not speculative holding. A "claim if you hold SRX" airdrop is incompatible with that positioning and would create regulatory and reputational drag in target markets.

Goals:
- Reward early builders and validators who took risk before mainnet was proven
- Bootstrap dApp ecosystem activity (quests, deploys, swaps)
- Distribute tokens to actual users, not speculators
- Establish auditable, on-chain distribution that survives external diligence

Non-goals:
- Maximize claim count (a 100K-wallet airdrop with 90% sybil farmers is worse than a 1K-wallet airdrop where every wallet earned it)
- Replace organic demand with airdrop liquidity
- Distribute to wallets that hold SRX without using it

## Five-phase rollout

| Phase | Allocation | Audience | Trigger |
|---|---|---|---|
| **1 — Testnet Heroes** | 1,000,000 SRX | Validators + power-users on chain 7120 (cumulative tx count ≥ N pre-snapshot) | Q2 2026 — operator-announced |
| **2 — Quest Campaign** | 1,000,000 SRX | Galxe / Zealy-style task completers (faucet, swap, contract-deploy quests) | Q3 2026 |
| **3 — Activity Rewards** | 800,000 SRX | Active mainnet wallets (tx velocity + balance retention metrics) | Q3 2026 |
| **4 — Validator Delegators** | 700,000 SRX | Pro-rata to delegators on active validators (snapshot height TBD) | Q4 2026 |
| **5 — Retroactive Builders** | 1,500,000 SRX | dApp deployers, audit contributors, ecosystem PRs (committee-reviewed) | Q4 2026 / Q1 2027 |

Each phase has its own snapshot, eligibility filter, and Merkle distribution. Phases run sequentially — completion of one does not gate the next, but earlier phases inform later snapshot designs (e.g., Phase 1 testnet activity may filter Phase 3 mainnet snapshots).

> **On the Phase 1 trigger:** earlier drafts of this doc gated Phase 1 on the upstream Chainlist listing (`ethereum-lists/chains#8266`) merging. That gate has been softened to "operator-announced." Reason: Chainlist listing is a conversion booster (one-click "Add Sentrix" in MetaMask, library-side auto-config in viem/ethers/wagmi), not a technical prerequisite. The claim page kicks `wallet_addEthereumChain` directly when the connected wallet isn't on chain 7119, so a Phase 1 launch before Chainlist merges still works — the recipient experience is two extra prompt clicks instead of one. The trade-off is conversion rate, not feasibility.

## Phase 1 — Testnet Heroes (design direction)

Phase 1 is in design phase. Specific parameters (tx-count threshold, wallet-age threshold, snapshot height, claim window length, per-wallet distribution amount) will be locked and announced together at Phase 1 launch — they are deliberately not pre-committed here so that the design can adapt to actual testnet activity patterns observed pre-launch.

### Eligibility direction

A wallet on testnet 7120 will be eligible if it shows evidence of **real participation** — not just faucet farming. Direction signals:

- Some minimum cumulative transaction count (threshold TBD at launch)
- Some minimum wallet age before the snapshot (threshold TBD at launch)
- At least one "real activity" signal — examples being considered:
   - Smart contract deployed on testnet
   - DEX swap (post DEX launch on testnet)
   - Operating as registered testnet validator
   - Verified completion via partner quest platform

The "real activity" requirement is the primary sybil filter. Faucet-only farming will not qualify.

### Snapshot height

Locked + announced at Phase 1 launch. Activity after the snapshot does not count.

### Distribution direction

- **Distribution recipient token: mainnet SRX, not testnet tSRX** — recipients claim mainnet 7119 token, even though eligibility is determined on testnet 7120. Reason: mainnet token is the only one with real economic value.
- **Per-wallet amount + tiering rules:** locked at launch (informed by snapshot eligible-pool size).
- **Claim window length:** locked at launch. Unclaimed SRX disposition (return to Strategic Reserve vs. roll into next phase vs. burn) will be specified at launch.

### Distribution mechanism (planned shape)

- Off-chain: snapshot eligibility → Merkle tree of `(address, amount)` leaves → publish root + leaf list (open data, anyone can verify inclusion)
- On-chain: deploy `MerkleAirdrop` claim contract on mainnet 7119, pre-fund with 1,000,000 SRX from Strategic Reserve in a single transparent transaction
- User flow: connect wallet → see eligibility → call `claim(proof, amount)` → receive SRX
- Sweep flow at end of claim window → unclaimed disposition per Phase 1 launch terms

The Merkle-claim contract has not been deployed yet. Design + audit precede deployment.

### Exclusion list (hard rule, no exceptions)

The following wallets are excluded from all airdrop phases regardless of activity signals:

- **Founder** — `0x5b5b06688dcdbe532353ac610aaff41af825279d` (already holds 21M premine; vesting Q3 2026)
- **Strategic Reserve** — `0x2578cad17e3e56c2970a5b5eab45952439f5ba97` (the airdrop source itself)
- **Ecosystem Fund** — `0xeb70fdefd00fdb768dec06c478f450c351499f14` (premine, separate operational allocation)
- **Early Validator** — `0x328d56b8174697ef6c9e40e19b7663797e16fa47` (premine, cold-storage allocation)
- **Authority** — `0xa25236925bc10954e0519731cc7ba97f4bb5714b` (governance signer — must not benefit from airdrop it gates)
- **Deployer** — `0x5acb04058fc4dfa258f29ce318282377cac176fd` (one-shot bootstrap deployer, retired)
- **Validator wallets** — Foundation, Treasury, Core, Beacon validator addresses (already earn V4 block rewards; airdrop would double-dip)
- **Faucet wallets** — mainnet + testnet faucet operator wallets (would self-loop the airdrop into faucet flows)
- **Compromised legacy wallets** — Founder v1, Founder v2 (drained, not in use, but explicit exclusion for clarity)

Specific addresses for validators + faucets are listed in the canonical addresses doc inside `sentrix-labs/canonical-contracts`.

### Sybil resistance direction

The eligibility filter is being designed to make sybil farming unattractive vs. expected reward. Layered filters under consideration:

- Wallet-age cutoff (filters wallets generated mass-style after announcement)
- Cumulative transaction-count threshold (raises farming cost)
- "Real activity" signal — contract deploy / DEX swap / validator operation / verified quest. This is the primary filter: it requires non-trivial action that is hard to script-farm at scale.
- Distribution shape (flat vs. tiered) — chosen to minimize the marginal value of farming many wallets vs. one organic wallet

We optimize for "every recipient earned this" over hitting a high claim count. The eligible pool is expected to be smaller-and-meaningful, not large-and-diluted.

## Phases 2–5 — design notes

Detailed mechanics will be published before each phase launches. Direction signals:

### Phase 2 — Quest Campaign (Q3 2026)

Quest-platform integration (Galxe, Zealy, or equivalent — final platform TBD). Quests designed around real protocol use (canonical contract interaction, DEX activity post-launch, etc.). Allocation per quest tier; rewards in mainnet SRX. Same exclusion list as Phase 1.

### Phase 3 — Activity Rewards (Q3 2026)

Mainnet snapshot of active wallets. Eligibility likely to combine signals such as transaction velocity over a measurement window, balance retention, and diversity of interactions. Specific weights + thresholds locked at snapshot.

### Phase 4 — Validator Delegators (Q4 2026)

Pro-rata distribution to delegators on active validators at snapshot height. Targets distributed staking participation. Specific eligibility rules (e.g., handling of validator self-stake) locked at snapshot.

### Phase 5 — Retroactive Builders (Q4 2026 / Q1 2027)

Committee-reviewed allocation for ecosystem contributors — dApp deployers, audit contributors, ecosystem PR authors, educational content creators. Tiered allocation. Committee composition will be specified before review begins.

## Auditability

Every airdrop transaction is observable on `scan.sentrixchain.com`:
- The single Strategic Reserve → MerkleAirdrop fund tx (pre-fund)
- Each user claim tx (recipient + amount)
- The post-window sweep tx (returning unclaimed funds)

The Merkle tree leaf list is published openly so anyone can:
1. Verify their inclusion (or absence)
2. Re-derive the Merkle root and confirm it matches the on-chain root
3. Audit the eligibility filter against historical chain state

## Risks and trade-offs

| Risk | Mitigation |
|---|---|
| Sybil farming despite filters | Multi-layer filter (wallet age + tx threshold + real activity) makes farming costlier than expected reward at small scale |
| Eligible pool smaller than expected | Acceptable. Per-recipient amount remains meaningful (2K+ SRX). A small-pool airdrop is still a credible community gesture. |
| Regulatory framing concerns | All phases tied to participation, not speculation. Distribution is opt-in claim, not push. Auditable on-chain. |
| Founder optics | Hard exclusion list — founder, validators, governance signers explicitly cannot claim. Disclosure upfront. |
| Unclaimed funds disposition | Returns to Strategic Reserve (not burned). Preserves supply curve in tokenomics. May be reallocated to subsequent phases. |

## Roadmap

| Quarter | Milestone |
|---|---|
| Q2 2026 | Phase 1 announcement (operator-announced; Chainlist listing nice-to-have for conversion but not gating); snapshot height locked at announcement |
| Q2 2026 | `MerkleAirdrop.sol` deploy + pre-fund (single tx from Strategic Reserve) |
| Q2 2026 | Phase 1 claim window opens |
| Q3 2026 | Phase 2 + Phase 3 launch (per tokenomics §6) |
| Q4 2026 | Phase 4 launch |
| Q4 2026 / Q1 2027 | Phase 5 retroactive committee review |

Quarter-level targets follow tokenomics §6. Specific dates within each quarter are not pre-committed.

## Cross-references

- [`OVERVIEW.md`](OVERVIEW.md) — full tokenomics (supply curve, premine, halving)
- [`docs/GOVERNANCE.md`](../GOVERNANCE.md) — multisig spending model
- [`docs/security/AUDIT_SUMMARY.md`](../security/AUDIT_SUMMARY.md) — chain audit posture
- Canonical contracts (incl. future `MerkleAirdrop.sol`): [`sentrix-labs/canonical-contracts`](https://github.com/sentrix-labs/canonical-contracts)
