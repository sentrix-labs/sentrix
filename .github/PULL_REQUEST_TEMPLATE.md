<!--
Thanks for the PR. Fill in the relevant sections + check the boxes that apply.
The risk-tier checklist below is the gate — it codifies discipline rules that
existed in `feedback_*.md` memory but kept getting skipped under time pressure.
-->

## Summary

<!-- 1-3 lines: what changed and why. Link the issue if applicable. -->

## Risk tier

Check ONE:

- [ ] **🟢 Low** — docs, tools/, tests/, CI configs, dependency patch bumps in dev-only crates, comments
- [ ] **🟡 Medium** — non-consensus production code (RPC handlers, network plumbing, observability, ops scripts)
- [ ] **🟠 High** — consensus-critical crates (`sentrix-core`, `sentrix-trie`, `sentrix-staking`, `sentrix-bft`), `block_executor`, `apply_block_*`, `state_root` path
- [ ] **🔴 Critical** — Voyager activation, fork-height changes, hard-fork rollouts, anything that flips env vars on mainnet

## Required by tier

### 🟢 Low — minimum bar
- [ ] CI green (tests + clippy + audit + gitleaks)

### 🟡 Medium — adds
- [ ] New public function or behavioural change has at least one corresponding `#[test]` in same PR
- [ ] Brief description of how this was tested (manual run, integration test, etc.)

### 🟠 High — adds
- [ ] Regression test that **fails on main** and **passes with this change** — paste test name in PR body
- [ ] Designed against documented invariant (link the audit/runbook/design doc)
- [ ] Fresh-brain review by someone other than the author (per `feedback_consensus_change_review`)
- [ ] Single conceptual unit per PR (no bundling — bundling consensus changes burned us on v2.1.12 → 2026-04-25 livelock)

### 🔴 Critical — adds
- [ ] **Testnet rehearsal completed** with success criteria + log evidence linked here
- [ ] **Bake window** observed: minimum 2h on testnet at the same configuration before mainnet
- [ ] **Coordinated rollback plan** documented in PR body — exact commands operator runs if it fails
- [ ] **Operator sign-off** at activation moment (not just PR approval — separate moment for the actual flip)

## Test plan

<!-- Bullet list. For 🟠 / 🔴 PRs the regression test must be specific. -->

-

## Rollback plan

<!-- Required for 🔴 PRs. Recommended for 🟠. The exact commands an on-call operator would run if this PR causes incidents post-deploy. -->

## Related

<!-- Issue numbers, prior PRs, design docs in founder-private/, etc. -->
