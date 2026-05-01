# Sentrix Security Audit Summary

**Last updated:** 2026-04-28

This document is the navigation hub for security material on Sentrix Chain. It is intended for:
- External auditors performing diligence
- Listing platforms (CG, CMC, exchanges)
- Researchers reviewing the chain's security posture
- Future contributors picking up the codebase cold

For technical detail, see the dedicated documents:

- [`SECURITY_AUDIT_V11.md`](SECURITY_AUDIT_V11.md) — most recent code review (39 files, ~6,500 LoC)
- [`SECURITY_REPORT.md`](SECURITY_REPORT.md) — earlier cumulative summary
- [`PENTEST_RESULTS.md`](PENTEST_RESULTS.md) — penetration test methodology + raw results
- [`ATTACK_VECTORS.md`](ATTACK_VECTORS.md) — threat model
- [`MULTISIG.md`](MULTISIG.md) — SentrixSafe technical specification

## Specialized audits

In addition to the numbered code-review rounds, several topical audits have been run:

| Topic | Date | Status |
|---|---|---|
| BFT consensus engine | 2026-04-20 | Reviewed; bugs found + fixed (BFT skip-round, justification-set divergence) |
| EVM integration & gas accounting | 2026-04-22 | Reviewed; reverted-tx state-leak bug fixed (PR #281), gas-cap EIP-7825 alignment fixed (v2.1.46) |
| Dependency supply chain | 2026-04-21 | `cargo audit` clean; CI runs `cargo audit` + `gitleaks` on every PR |
| CI/CD security posture | 2026-04-21 | Reviewed; secret-scanning + signed-commit verification active |
| Validator infrastructure security | 2026-04-21 | Reviewed; SSH-key custody, validator host hardening documented in operator runbooks |
| Tokenomics correctness | 2026-04-25 | Reviewed; supply invariants hold across all forks |
| BFT skip-round root cause | 2026-04-28 | Phase 2 RCA documented in operator runbooks |

## External audit posture

No third-party audit firm has reviewed Sentrix Chain code as of 2026-04-28. External audit is something we'd pursue when budget + scope align — no committed timeline.

The chain runs continuous internal review:
- `cargo audit` + `gitleaks` on every PR
- `slither` + `mythril` on Solidity contracts (CI gate)
- Manual code review by the internal Sentrix Labs / SentrisCloud security team for every PR
- Public bug bounty: see [SECURITY.md](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md) (safe-harbor policy in effect)

Listing platforms or external auditors performing diligence: contact `security@sentriscloud.com` for code-walkthrough or audit-prep discussion.

## How to report

If you find a security issue:

1. **Do not open a public GitHub issue.**
2. Email `security@sentriscloud.com` with details.
3. Include reproduction steps, impact assessment, and suggested fix if applicable.
4. We acknowledge within 48 hours; remediation timeline depends on severity.
5. Safe-harbor policy applies — researchers acting in good faith are protected from legal action; see [SECURITY.md](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md) for full terms.

## Cross-references

- [`SECURITY.md`](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md) — safe-harbor + reporting policy
- [`SECURITY_REPORT.md`](SECURITY_REPORT.md) — earlier cumulative summary
- [`SECURITY_AUDIT_V11.md`](SECURITY_AUDIT_V11.md) — most recent round, full detail
- [`ATTACK_VECTORS.md`](ATTACK_VECTORS.md) — threat model
- [`PENTEST_RESULTS.md`](PENTEST_RESULTS.md) — pentest methodology + outcomes
- [`MULTISIG.md`](MULTISIG.md) — SentrixSafe technical specification
- [GOVERNANCE.md](../GOVERNANCE.md) — control / governance model
