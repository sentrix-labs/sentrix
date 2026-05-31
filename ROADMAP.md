# Sentrix Chain Roadmap

> Last updated 2026-05-31. Subject to change via the [SIP process](https://github.com/sentrix-labs/SIPs).
>
> Current focus: **consensus-stability fixes** ([SIP-5](https://github.com/sentrix-labs/SIPs/pull/2), [SIP-6](https://github.com/sentrix-labs/SIPs/pull/3)) before the next feature wave.

## Status

- 🔴 **Mainnet**: temporarily off pending architectural fixes (SIPs #5 + #6 in review)
- 🟢 **Testnet**: running 3-of-4 BFT at v2.2.23. Chain producing blocks normally. One validator (val3) jailed since 2026-05-30 — recovery blocked by Bug A fix
- 🟢 **Bridges**: Hyperlane v3 functional testnet → Sepolia (testnet-grade security, production hardening in progress)
- 🟢 **Tooling**: explorer, faucet, RPC, gRPC, dashboards all live

---

## Recently shipped

Past 30 days (mainnet + testnet):

- **Tokenomics v2** activated mainnet — 315 M cap, 4-year halving (BTC-parity), 63 M premine ratio improves to 20% ([SIP-3](https://github.com/sentrix-labs/SIPs/blob/main/sips/sip-3.md))
- **v2.2.19** apply-path observability — `apply_watchdog` instrumentation surfaces silent-stall classes before they cascade
- **v2.2.20** BFT engine self-heal — engine resume from persisted last-sign at restart, closes round-0-step-2 stuck pattern
- **v2.2.21** `BftMetrics` + `/metrics` endpoint — Prometheus surface for round duration, vote tally, phase timeouts, jail events
- **v2.2.22** `sentrix staking` CLI — TX-based `register` / `add-self-stake` / `unjail` / `claim-rewards` via `apply_block` (no DB-edit trap)
- **v2.2.23** receiver-side `block.hash == justification.block_hash` consistency check (gated by strict-justification fork)
- **revm 40** EVM engine upgrade
- **Hyperlane v3 testnet bridge** — Sentrix Testnet ↔ Sepolia, manual relay, NoopIsm (production hardening pending)
- **Signed releases**: cosign keyless OIDC + SLSA L3 provenance on every release tag
- **Docs consolidation**: [docs.sentrixchain.com](https://docs.sentrixchain.com) as single source of truth — validator onboarding runbooks, slashing parameters synced to code

## Current focus

Q2 2026 priority order:

1. **Bug A fix** — off-trie consensus state (pending_rewards, total_minted, liveness, epoch) moved into state trie. Eliminates silent drift class. ([SIP-6](https://github.com/sentrix-labs/SIPs/pull/3))
2. **Bug B fix** — block hash + state_root commitment consistency via speculative apply. ([SIP-5](https://github.com/sentrix-labs/SIPs/pull/2))
3. **Mainnet restart** — coordinated halt-all + simul-start window after Bug A + Bug B activate on testnet + bake clean ≥1 week
4. **Multi-key validator separation** — signer / operator / stash key split, replaces current one-key model
5. **Public validator onboarding** — externalize the runbook + UX once consensus-stability fixes ship and slashing economics are bake-tested

## Planned next

After Q2 stability sprint:

- **Validator monitoring templates** — public Grafana dashboards for `bft_*` and `apply_*` series
- **DEX subgraph** — UniV2-fork subgraph for `sentrix-dex` pools + swaps
- **NFT primitive** — Seaport-compatible marketplace on top of EVM
- **LayerZero V2** — production DVN + Executor wiring after Labs assignment
- **Slashing bake** — testnet validator misbehavior testing against live slashing rates

## Long-term direction

Conceptual, no timeline commitments:

- **Parallel apply** — block-level parallelism via read-write set scheduler
- **DAG consensus exploration** — Narwhal/Bullshark-style mempool dissemination + commit
- **Cross-chain liquidity** — Cosmos IBC integration evaluated if Cosmos-side demand emerges
- **Validator set scaling** — 1000+ validators (BFT message O(N²) reduction, gossipsub fanout tuning)
- **Bug bounty program** — public Immunefi-style after architectural fixes land

---

## How decisions are made

- **Code changes**: PR review on [sentrix-labs/sentrix](https://github.com/sentrix-labs/sentrix)
- **Consensus rule changes**: [SIP process](https://github.com/sentrix-labs/SIPs/blob/main/sips/sip-1.md) — design doc → review → testnet bake → mainnet activation height
- **Economic parameters**: SIP track with tokenomics rationale
- **No on-chain governance** today. SIP process is documentation + review; final acceptance via maintainers + validators

## Where to plug in

- **Validator**: see [Validator Onboarding](https://docs.sentrixchain.com/docs/operations/VALIDATOR_ONBOARDING)
- **dApp dev**: [Integration Cookbook](https://docs.sentrixchain.com/docs/operations/INTEGRATION_COOKBOOK)
- **Bug reports**: [GitHub issues](https://github.com/sentrix-labs/sentrix/issues)
- **Protocol proposals**: [SIPs repo](https://github.com/sentrix-labs/SIPs)
- **Security disclosures**: see [SECURITY.md](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md)
