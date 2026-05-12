<!-- Brief one-line title above. -->

## Summary

What changed and why. 1-3 sentences.

## Scope

- [ ] Contract change (new file or logic edit) — needs slither pass + audit consideration
- [ ] Test-only change
- [ ] Deploy script / CI / docs only
- [ ] Repo tooling (Makefile, lefthook, etc.)

## Checks

- [ ] `forge build` clean
- [ ] `forge test` green (including fuzz + invariant)
- [ ] `forge fmt --check` clean
- [ ] `slither --no-fail-pedantic` reviewed (no new high-severity findings)
- [ ] Storage layout diff reviewed if any contract field added/removed (run `make storage` and inspect `docs/storage/*.json`)

## Linked issue

Closes #

## Deploy impact

- [ ] No on-chain change (test/CI/docs only)
- [ ] Requires v-bump + redeploy when shipped
- [ ] Backwards-compatible (extends, doesn't reorder storage)
- [ ] Breaking — needs migration plan in `RELEASES.md`
