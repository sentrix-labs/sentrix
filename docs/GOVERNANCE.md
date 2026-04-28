# Sentrix Chain Governance

**Last updated:** 2026-04-28

This document describes how decisions are made on Sentrix Chain — who controls what, how spending happens, and how the model evolves over time.

## TL;DR

- **Treasury control:** SentrixSafe multisig contracts (Gnosis Safe-style) deployed on both chains. Currently 1-of-1 with the Authority key as sole owner. Multi-sig expansion (adding owners + raising threshold) is possible at any time via on-chain `addOwner()` + `changeThreshold()`, but no specific expansion timeline is committed.
- **Protocol upgrades:** Hard forks gated by build-time constants + height triggers. Validators must adopt the upgraded binary; misalignment results in self-fork (no committee veto).
- **Audit / change log:** every governance-relevant transaction (multisig spend, fork activation, premine outflow) is publicly visible on `scan.sentrixchain.com`.

## 1. Treasury governance

### Strategic Reserve & Ecosystem Fund

Two main pools hold protocol-level capital:

| Pool | Allocation | Purpose | Control mechanism |
|---|---|---|---|
| Strategic Reserve | 10,500,000 SRX | Airdrop campaign (5M), CEX listing fees (3M), DEX bootstrap liquidity (1.5M), emergency (1M) | EOA wallet, key held by Authority (same operator who controls SentrixSafe). Social custody — on-chain enforcement (Reserve owned by SentrixSafe contract) is acknowledged as a roadmap item without committed timeline. |
| Ecosystem Fund | 21,000,000 SRX | Operational ops: faucet refill, marketing, bounties, dev grants | EOA wallet, key held by Authority. Same custody model as Strategic Reserve. |

Sub-bucket targets are **policy-level commitments** (documented in this repo and on `sentrixchain.com/docs/tokenomics`). They are **not on-chain enforced** — they are auditable via on-chain transaction history.

Rationale for soft-policy vs. hard-on-chain enforcement: flexibility for emergent ecosystem needs (e.g., reallocating from CEX listings to DEX liquidity if listing timeline shifts) without requiring a hard fork or contract upgrade. Trade-off: requires public commitment + auditable on-chain trail to maintain trust.

### SentrixSafe — multisig contract

[`SentrixSafe.sol`](https://github.com/sentrix-labs/canonical-contracts/blob/main/contracts/SentrixSafe.sol) is a minimal Gnosis Safe v1.4.1-style multisig:

- N-of-M owner threshold
- EIP-712 typed signature verification
- On-chain owner management (`addOwner`, `removeOwner`, `changeThreshold`)
- No modules, no guards, no fallback handlers (smaller attack surface vs. full Gnosis Safe)

**Mainnet (chain 7119):** `0x6272dC0C842F05542f9fF7B5443E93C0642a3b26` (deployed at block 717618)
**Testnet (chain 7120):** `0xc9D7a61D7C2F428F6A055916488041fD00532110` (deployed at block 723511)

### Current state — 1-of-1 (bootstrap)

As of 2026-04-28, SentrixSafe runs **1-of-1**:

- Sole owner: Authority address `0xa25236925bc10954e0519731cc7ba97f4bb5714b`
- Threshold: 1 (single signature required)
- Effectively functions as a single-signer wallet **with multisig contract shape** ready for expansion

Why deploy multisig if effectively single-sig? Three reasons:
1. **Migration path exists** — adding owners + raising threshold requires only `addOwner()` + `changeThreshold()` calls. No fund migration, no contract redeploy.
2. **Auditable trail** — every spend is an `execTransaction()` event, distinct from a regular EOA transfer. Easier to grep for treasury-relevant tx history.
3. **Reputational signal** — listing platforms, partners, and external auditors get clearer governance structure than "founder EOA holds everything."

**Deployer retirement:** the bootstrap deployer EOA (`0x5acb04058fc4dfa258f29ce318282377cac176fd`) was added as initial owner to deploy SentrixSafe, then removed via `removeOwner()` once the Authority took over. Migration history:
- Mainnet: `addOwner(authority)` at block 755821 → `removeOwner(deployer)` at block 757829
- Testnet: `addOwner(authority)` at block 881639 → `removeOwner(deployer)` at block 884599

Tx hashes available in [`canonical-contracts/docs/ADDRESSES.md`](https://github.com/sentrix-labs/canonical-contracts/blob/main/docs/ADDRESSES.md).

### Multi-sig expansion (no committed timeline)

The contract is shape-ready for N-of-M expansion. Adding owners + raising threshold is a same-day on-chain operation:

```
addOwner(<new_signer_address>)   // repeat per signer
changeThreshold(<new_threshold>) // raise threshold
```

Expansion will happen when independent signers (advisors, security council members) are recruited and onboarded. No timeline is committed because committing to a specific quarter without recruited signers in pipeline would be performative — a 3-of-5 wallet with non-responsive co-signers is worse than a working 1-of-1.

The 1-of-1 setup is honest about Sentrix's current operational stage. The signal we want to send to listing platforms and partners is: governance contract exists + ready to expand, not "we will definitely have 5 signers by date X."

## 2. Protocol upgrades (hard forks)

### Mechanism

Protocol upgrades are gated by **build-time constants** + **height triggers**. The relevant constants live in `sentrix-primitives::constants`:

- `VOYAGER_FORK_HEIGHT` — DPoS+BFT activation
- `VOYAGER_EVM_HEIGHT` — EVM execution activation
- `VOYAGER_REWARD_V2_HEIGHT` — V2 reward distribution
- `TOKENOMICS_V2_HEIGHT` — 315M supply cap + 4-year halving (replaces 210M / 1.33-year)
- `BFT_GATE_RELAX_HEIGHT` — relaxed BFT activation threshold
- `ADD_SELF_STAKE_HEIGHT` — `StakingOp::AddSelfStake` opcode availability
- `JAIL_CONSENSUS_HEIGHT` — full jail-evidence consensus dispatch (currently dormant)
- `NFT_TOKENOP_HEIGHT` — SRC-721 / SRC-1155 native NFT TokenOps (Pass-1 gate shipped, Pass-2 dispatch pending)

Each fork is **opt-in by binary version**: validators running the upgraded binary apply the new rule at the trigger height; validators on older binaries reject blocks containing the new behavior and self-fork (chain partition).

### Validator coordination

For an upgrade to succeed without partition:
1. Binary release published with new constants
2. Validators independently upgrade their nodes (operator coordination, off-chain)
3. Trigger height passes
4. ≥2/3 voting power running new binary → chain advances
5. Validators on stale binary either upgrade or remain forked (jailed by lack of liveness)

There is **no on-chain governance vote** for protocol upgrades in the current model. This is a deliberate choice for the current bootstrap phase:
- 4-validator network — voting would be performative
- 6 fork activations have shipped to date (see Past activations table below); 2 notable recovery incidents occurred (Voyager livelock 2026-04-25 — peer-mesh partition; cascade-jail 2026-04-28 — chain.db state divergence post deploy). Both resolved via halt-all + chain.db rsync from canonical state. No invalid block was ever accepted; no funds were lost.
- Future: once external validator onboarding scales beyond Foundation set, on-chain governance for upgrades is a roadmap consideration (no committed timeline)

### Past activations

| Fork | Height | Date | Notes |
|---|---|---|---|
| `VOYAGER_FORK_HEIGHT` | 579047 | 2026-04-25 | DPoS+BFT activation (replaced Pioneer PoA round-robin) |
| `VOYAGER_EVM_HEIGHT` | 579060 | 2026-04-25 | EVM execution live |
| `VOYAGER_REWARD_V2_HEIGHT` | 590100 | 2026-04-26 | V2 reward distribution |
| `TOKENOMICS_V2_HEIGHT` | 640800 | 2026-04-26 | 315M cap + 4-year halving |
| `BFT_GATE_RELAX_HEIGHT` | 692700 | 2026-04-27 | Lowered BFT activation threshold |
| `ADD_SELF_STAKE_HEIGHT` | 731245 | 2026-04-28 | Validator self-bond opcode |

## 3. Validator set governance

### Active set selection

The active validator set is determined per-epoch by:
1. `RegisterValidator` opcode (anyone can submit, requires minimum self-stake)
2. `Delegate` opcode (anyone can delegate to any registered validator)
3. Active set = top-N by total stake (self-stake + delegated), where N is `ACTIVE_SET_SIZE`

There is **no permissioned validator allow-list** at the protocol layer. Anyone meeting the minimum self-stake can register and compete for active-set slots.

### Slashing & jailing

`StakingOp::JailEvidenceBundle` (post `JAIL_CONSENSUS_HEIGHT` activation, currently dormant) allows any validator to submit evidence of:
- Double-signing (Byzantine fault)
- Liveness violations (missed blocks above threshold)

The slashing engine deterministically:
1. Verifies evidence against signed block headers
2. Burns slashed self-stake
3. Marks validator as jailed (skipped in active-set selection)

A jailed validator can `Unjail` after a cooldown by:
- Submitting fresh self-stake via `AddSelfStake` to clear shortfall
- Calling `Unjail` opcode (verifies cooldown elapsed + stake restored)

This design — anyone can submit evidence — replaces the typical "permissioned slasher" model. There is no governance veto over evidence; the protocol decides deterministically.

## 4. Emergency response

### What's auditable in real time

Every governance-relevant action is observable on `scan.sentrixchain.com`:
- Treasury spends (filterable by `from = SentrixSafe address`)
- Strategic Reserve outflows (filterable by `from = 0x2578cad17e3e56c2970a5b5eab45952439f5ba97`)
- Validator set rotations (epoch-boundary events)
- Fork activations (block-by-block constant-evaluation context)

WebSocket subscriptions stream these events live (see [WebSocket Subscriptions](operations/WEBSOCKET_SUBSCRIPTIONS.md)).

### Operator response

Mainnet incidents (validator jailing, livelock, chain.db divergence) are handled by operator runbooks. The recovery model is:

1. **Halt-all** — all validators stop simultaneously to prevent state divergence during recovery
2. **Canonical state selection** — pick a healthy validator's chain.db as ground truth
3. **Rsync** — broadcast canonical chain.db to other validators
4. **Simul-start** — all validators start within a 1-2 second window

This pattern is documented in operator runbooks. Recovery MTTR has improved over time as tooling matured: earlier incidents (Voyager livelock 2026-04-25, BFT cascade events) took 30+ minutes. Recent incidents (cascade-jail recovery 2026-04-28) recovered in approximately 3 minutes via halt-all + chain.db rsync. Trend is improving; fully-automated recovery tooling is a roadmap item.

There is no governance vote for emergency response; the operator coordinates. Once the validator set decentralizes beyond the Foundation, this model will need a community-aware variant.

## 5. Roadmap — governance evolution

| Quarter | Milestone |
|---|---|
| Q2 2026 | Foundation-operated 1-of-1 SentrixSafe, hardcoded fork gating, off-chain operator coordination |
| Q3 2026 | Founder-vesting contract deploy (locks §2a on-chain) per tokenomics §9 |
| Future (no committed timing) | SentrixSafe multi-sig expansion — when independent signers are recruited |
| Future (no committed timing) | External validator onboarding — current 4 validators are Foundation-operated; criteria, cadence, and timing all TBD when operator-readiness framework is finalized |
| Future (no committed timing) | On-chain governance for protocol upgrades — proposal + voting framework, mechanism TBD |
| Future (no committed timing) | Decentralized treasury governance (DAO-style), replacing Foundation-coordinated multisig |

The trajectory is intentional: bootstrap with concentrated control for safety + speed, decentralize as community + tooling matures. No "instant DAO at launch" — that's a recipe for unmaintained governance.

## 6. Cross-references

- Tokenomics & supply: [`docs/tokenomics/OVERVIEW.md`](tokenomics/OVERVIEW.md)
- Audit summary: [`docs/security/AUDIT_SUMMARY.md`](security/AUDIT_SUMMARY.md)
- Airdrop mechanics: [`docs/tokenomics/AIRDROP_MECHANICS.md`](tokenomics/AIRDROP_MECHANICS.md)
- Canonical contracts (incl. SentrixSafe source): [`sentrix-labs/canonical-contracts`](https://github.com/sentrix-labs/canonical-contracts)
- Multi-sig technical doc: [`docs/security/MULTISIG.md`](security/MULTISIG.md)
