# Release Process

## Versioning

Sentrix follows [Semantic Versioning](https://semver.org/):
- **MAJOR** — consensus-breaking changes (hard fork required)
- **MINOR** — new features, backward-compatible protocol changes
- **PATCH** — bug fixes, performance improvements

## Release Checklist

1. **Update version** in `Cargo.toml`
2. **Update CHANGELOG.md** — move items from `[Unreleased]` to new version section
3. **Run full test suite** — `cargo test` (all tests must pass)
4. **Run clippy** — `cargo clippy --tests -- -D warnings` (must be clean)
5. **Create PR** — merge to `main` via PR with CI passing
6. **Tag release** — `git tag -a vX.Y.Z -m "vX.Y.Z"` then `git push origin vX.Y.Z`
7. **GitHub Release** — create release from tag with changelog excerpt
8. **Deploy** — CI/CD `deploy` job is **disabled**. Run
   `./scripts/fast-deploy.sh mainnet` from VPS4 (or `testnet` for
   testnet) to ship the binary; CI runs tests only. Then check health
   on all 3 VPS.

## Deployment

Primary path: **`scripts/fast-deploy.sh`** (runs from VPS4). Builds
inside a `rust:1.95-bullseye` container (glibc 2.31, compatible with
both 22.04 and 24.04 targets), uploads the binary to VPS1/VPS2/VPS3
via wg1 SCP, and does a rolling restart with a bounded health check.
~3–5 minutes end-to-end.

```bash
./scripts/fast-deploy.sh mainnet          # asks for confirmation
./scripts/fast-deploy.sh testnet          # silent
SENTRIX_ROLLBACK=/opt/sentrix/releases/<prev> \
  ./scripts/fast-deploy.sh mainnet        # instant rollback
```

CI still runs tests on every PR (for audit trail) but the GitHub
Actions `deploy` job is disabled — `fast-deploy.sh` is the only path
that ships a binary to prod. This avoids the race where both CI and
`fast-deploy` would redeploy the same commit.

Break-glass: **`scripts/emergency-deploy.sh`** skips the preflight
test gate and requires a strict confirmation phrase. Use only when
GitHub Actions is down, chain has halted, or an exploit needs a
bypass of the normal regression gate.

## Hotfix Process

1. Branch from `main`
2. Fix + test
3. PR with `fix(scope):` commit message — auto-merge on green CI
4. Run `./scripts/fast-deploy.sh mainnet` from VPS4 after merge
