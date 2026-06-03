# Internal audits

Internal RCAs, design docs, and ops investigations. **NOT external auditor reports** — those would land elsewhere with a clear cover sheet identifying the auditor, scope, and engagement period.

The files here are operator-side analysis documents written during incident response or design exploration. They're checked in for posterity (so the reasoning behind specific PRs and forks is recoverable) and to give future contributors context for why certain code paths look the way they do.

## What lives here

- **Incident root-cause analyses** — `*-root-cause-analysis.md`, `jail-cascade-*`, etc. Written shortly after the incident, references commits + log lines + recovery steps.
- **Design docs for non-trivial changes** — `consensus-computed-jail-design.md`, etc. Pre-implementation reasoning for decisions that touched consensus rules.
- **Fork activation playbooks** — `native-state-in-trie-activation-playbook.md`, etc. Pre-flight + halt-all/simul-start + monitoring + rollback procedures for turning on a fork-gated consensus change without splitting the chain.
- **Audit-style code reviews** — `codebase-areas-*-audit.md`, `libp2p-resilience-audit-*`, `reward-distribution-flow-audit-*`. Internal reviews to surface concerns before they became incidents.
- **Production-readiness reviews** — `sentrix-production-readiness-audit-*`. Snapshot assessments of where the chain stood vs production-ready criteria at a given moment.

## What does NOT live here

- **External auditor reports.** When a third-party firm audits Sentrix, their report ships with a clearly identified cover sheet and lands either alongside (with a name like `external-audit-<firm>-<date>.md`) or in a separate top-level folder.
- **Operator runbooks.** Day-to-day ops procedures live with the operator, not in the public repo.
- **Daily operational status.** Session handoffs, deploy logs, and short-lived investigation notes are private; only durable design + incident artifacts surface here.

## Reading order for newcomers

If you're reading these to understand how the chain evolved:

1. `sentrix-production-readiness-audit-2026-04-27.md` — best snapshot of the chain's state and the open issue queue at the time of the most consequential design pass.
2. `codebase-areas-1-7-audit-2026-04-27.md` — bug hunt across 7 code areas; many of the findings became the next quarter's PRs.
3. `jail-cascade-root-cause-analysis.md` then `consensus-computed-jail-design.md` then `consensus-jail-phase-d-scoping.md` — three-doc arc on the consensus-jail problem class.
4. `libp2p-resilience-audit-2026-04-27.md` — networking-layer review.
5. `reward-distribution-flow-audit-2026-04-27.md` — staking + reward-flow correctness review that informed the V4 reward fork.

## Cross-references

Several of these documents reference other audit files (e.g. `audits/2026-04-30-eager-write-investigation.md`, `audits/bft-signing-fork-design.md`) that have been removed. Those references are historical breadcrumbs to documents that were superseded by code changes; the rationale they captured lives in the `CHANGELOG.md` entries that cite them.
