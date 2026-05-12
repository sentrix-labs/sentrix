---
name: Deployment request
about: Request a coordinated deploy (testnet + mainnet) of a canonical contract
title: "[deploy] vX.Y.Z — "
labels: deployment
---

**Target version**

`vX.Y.Z` (matches `VERSION` file at deploy time)

**Contracts in this deploy**

- [ ] WSRX
- [ ] Multicall3
- [ ] SentrixSafe
- [ ] TokenFactory
- [ ] (new contract — link to feature-request issue)

**Pre-deploy checklist**

- [ ] All contracts in scope have green CI on `main`
- [ ] Slither passes (`make lint`)
- [ ] Tests pass (`forge test`)
- [ ] `docs/AUDIT.md` reviewed; if PENDING, founder sign-off captured below
- [ ] Deployer wallet funded:
  - Testnet (chain 7120): ≥ 0.5 SRX
  - Mainnet (chain 7119): ≥ 0.5 SRX
- [ ] Deployer keystore password rotated from placeholder
- [ ] Foundry installed on the build host

**Deploy plan**

1. Testnet first (chain 7120) — record addresses
2. Smoke test via `forge script script/CheckDeployment.s.sol`
3. Mainnet (chain 7119) — same order
4. Update `deployments/{7119,7120}.json` + run `./script/copy-abi.sh` + `./script/GenerateAddressDocs.sh`
5. Update `CHANGELOG.md` `[vX.Y.Z]` section
6. Append to `RELEASES.md` table
7. `git tag vX.Y.Z && git push --tags` → `release.yml` auto-creates GH Release

**Rollback plan if mainnet deploy goes wrong**

- Halt all 4 mainnet validators per operator chain-halt runbook
- Investigate root cause
- Patch + redeploy as `vX.Y.Z+1`
- Migration advisory in `RELEASES.md`

**Founder sign-off**

- [ ] Reviewed contract diffs since last release
- [ ] Audit status acceptable for this risk profile
- [ ] Approve deploy

Signed-off by: @satyakwok on YYYY-MM-DD
