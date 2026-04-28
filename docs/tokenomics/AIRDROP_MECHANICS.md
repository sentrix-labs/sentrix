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
| **1 — Testnet Heroes** | 1,000,000 SRX | Validators + power-users on chain 7120 (cumulative tx count ≥ N pre-snapshot, wallet age ≥ 30 days) | Q2 2026, post-Chainlist listing |
| **2 — Quest Campaign** | 1,000,000 SRX | Galxe / Zealy-style task completers (faucet, swap, contract-deploy quests) | Q3 2026 |
| **3 — Activity Rewards** | 800,000 SRX | Active mainnet wallets (tx velocity + balance retention metrics) | Q3 2026 |
| **4 — Validator Delegators** | 700,000 SRX | Pro-rata to delegators on active validators (snapshot height TBD) | Q4 2026 |
| **5 — Retroactive Builders** | 1,500,000 SRX | dApp deployers, audit contributors, ecosystem PRs (committee-reviewed) | Q4 2026 / Q1 2027 |

Each phase has its own snapshot, eligibility filter, and Merkle distribution. Phases run sequentially — completion of one does not gate the next, but earlier phases inform later snapshot designs (e.g., Phase 1 testnet activity may filter Phase 3 mainnet snapshots).

## Phase 1 — Testnet Heroes (detailed mechanics)

This is the imminent phase. Detailed mechanics for later phases will be added as their snapshot dates approach.

### Eligibility filter

A wallet on testnet 7120 is eligible if **all** of the following hold at the snapshot height:

1. **Cumulative tx count ≥ 50** — meaningful interaction, not faucet-claim-only
2. **Wallet age ≥ 30 days** before snapshot — filters mass wallet generation in the days preceding announcement
3. **At least one of the following "real activity" signals:**
   - Deployed at least one smart contract on testnet
   - Completed at least one DEX swap (once DEX is live on testnet)
   - Operated as a registered validator on testnet for ≥ 7 days
   - Verified via partner platform (Galxe / Zealy / Sentrix-quest portal — TBD)

The "real activity" requirement is the primary sybil filter. Faucet-only farming (claim 10M tSRX, sit, claim airdrop) does not qualify.

### Snapshot height

Target: chain 7120 height **400,000** (estimated 2 weeks after public Phase 1 announcement). Exact height locked at announcement; activity after the snapshot does not count.

### Distribution

- **Flat allocation per eligible wallet** — all eligible wallets receive equal share. If 500 wallets qualify, each receives 2,000 SRX.
- **No tiering** — Phase 1 is a community-trust signal (equal treatment for "passed the filter"). Tiering arrives in Phase 5 (Retroactive Builders) where merit genuinely differs.
- **Mainnet SRX, not testnet tSRX** — recipients claim mainnet 7119 token, even though eligibility is determined on testnet 7120. Reason: mainnet token is the only one with real economic value.
- **Claim window: 90 days** from contract deploy. Unclaimed SRX returns to Strategic Reserve (does not burn) — preserves the supply curve in `docs/tokenomics/OVERVIEW.md`.

### Distribution mechanism

- Off-chain: snapshot eligibility → Merkle tree of `(address, amount)` leaves → publish root + leaf list (open data, anyone can verify their inclusion)
- On-chain: deploy `MerkleAirdrop` claim contract on mainnet 7119, pre-fund with 1,000,000 SRX from Strategic Reserve in a single transparent transaction
- User flow: connect wallet → see eligibility → call `claim(proof, amount)` → receive SRX
- Sweep flow: after 90-day window, owner calls `sweep()` → unclaimed balance returns to Strategic Reserve

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

### Sybil resistance summary

The eligibility filter is structured to make sybil farming unattractive:

- **30-day wallet age** filters mass-generated wallets in announcement window
- **50-tx threshold** raises cost of mass farming
- **Real activity signal** (contract deploy / DEX swap / validator op / verified quest) requires non-trivial action that's hard to script-farm at scale
- **Flat distribution** removes the incentive to fragment farming across many wallets (1 farmed wallet = same as 1 organic wallet)

The result is a smaller eligible pool (~500 wallets expected for Phase 1, may be lower) but each recipient receives a meaningful 2,000 SRX. We optimize for "every recipient earned this" over "we hit a high claim count."

## Phases 2–5 — design notes

Detailed mechanics will be published before each phase launches. Direction signals:

### Phase 2 — Quest Campaign (Q3 2026)

Galxe / Zealy / equivalent task platform integration. Tasks designed around real protocol use:
- "Wrap 100 SRX → unwrap → record txid" (WSRX integration)
- "Deploy a SRC-20 token via TokenFactory" (TokenFactory integration)
- "Add liquidity to SRX/USDC pool on Sentrix DEX" (post-DEX-launch)

Allocation per quest tier; rewards in mainnet SRX. Same exclusion list as Phase 1.

### Phase 3 — Activity Rewards (Q3 2026)

Mainnet snapshot. Eligibility weighted by:
- Tx velocity over a measurement window (e.g., last 90 days)
- Balance retention (avoiding "claim and dump" pattern)
- Diverse interactions (not just SRX transfers — count contract calls)

Distribution may be tiered (proportional to score) rather than flat. Final mechanic locked at snapshot.

### Phase 4 — Validator Delegators (Q4 2026)

Pro-rata distribution to delegators on active validators at snapshot height. Validator self-stake does not count (validators already earn V4 rewards). Targets distributed staking participation.

### Phase 5 — Retroactive Builders (Q4 2026 / Q1 2027)

Committee-reviewed allocation for:
- dApp deployers who shipped real users (DEX, NFT marketplace, game, etc.)
- External audit contributors (third-party audits with public reports)
- Ecosystem PRs to canonical-contracts, sentrix repo, frontend monorepo
- Educational content creators (verified, non-trivial)

Tiered allocation. Committee composition: SentrixSafe authority + 2–3 community members (TBD post-multisig migration).

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

| Date | Milestone |
|---|---|
| Q2 2026 | Phase 1 announcement (post-Chainlist listing); snapshot height locked + announced |
| Q2 2026 | `MerkleAirdrop.sol` deploy + pre-fund (single tx from Strategic Reserve) |
| Q2 2026 | Phase 1 claim window opens (90 days) |
| Q3 2026 | Phase 1 sweep + Phase 2 / 3 launch |
| Q4 2026 | Phase 4 launch (post-DEX activity meaningful) |
| Q4 2026 / Q1 2027 | Phase 5 retroactive committee review |

## Cross-references

- [`OVERVIEW.md`](OVERVIEW.md) — full tokenomics (supply curve, premine, halving)
- [`docs/GOVERNANCE.md`](../GOVERNANCE.md) — multisig spending model
- [`docs/security/AUDIT_SUMMARY.md`](../security/AUDIT_SUMMARY.md) — chain audit posture
- Canonical contracts (incl. future `MerkleAirdrop.sol`): [`sentrix-labs/canonical-contracts`](https://github.com/sentrix-labs/canonical-contracts)
