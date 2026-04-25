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
8. **Deploy** — CI/CD `deploy` job is **disabled**. Operators run their
   own deploy orchestrator from a build host (binary built once, pushed
   to all validators, rolling restart, health-check). Then verify chain
   advance on every validator.

## Deployment

CI runs tests on every PR for audit trail but does **not** ship binaries
to validators. This avoids the race where both CI and an operator deploy
would redeploy the same commit.

**For third-party validators:** use `scripts/deploy-validator.sh` (the
generic single-validator primitive — takes SSH key, host, service,
bin_dir, RPC URL, binary path). It uploads the binary, archives the
previous version, restarts the service, and verifies health.

```bash
./scripts/deploy-validator.sh \
  --ssh-key  ~/.ssh/my_operator_key \
  --host     operator@validator.example.com \
  --service  sentrix-node \
  --bin-dir  /opt/sentrix \
  --rpc-url  http://127.0.0.1:8545 \
  --binary   ./target/release/sentrix
```

Wrap it in your own loop / Ansible play / k8s rollout for multi-validator
fleets.

**For maintainer mainnet operations:** orchestration lives in the
maintainer's private operations repo; it builds once in a
`rust:1.95-bullseye` container (glibc 2.31, compatible with all current
target distros), uploads binaries to all maintainer-fleet validators
over a private wireguard network, and does a rolling restart with
bounded health checks. The script is private because it bakes in
operator-specific infrastructure (wg1 IPs, role mapping, SSH key paths)
that aren't useful to anyone running their own validator.

## Rollback

Each validator keeps the last 3 binaries archived under
`<bin_dir>/releases/`. Roll back by re-running your deploy with the
prior archived binary instead of a fresh build.

## Hotfix Process

1. Branch from `main`
2. Fix + test
3. PR with `fix(scope):` commit message — auto-merge on green CI
4. Operator runs deploy after merge to ship the patched binary
