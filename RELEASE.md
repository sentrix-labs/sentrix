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
8. **Verify deployment** — CI/CD deploys automatically; check health on all 3 VPS

## Deployment

Deployment is fully automated via CI/CD (GitHub Actions). **Never deploy manually via SSH.**

Pipeline: Push to `main` → Test → Build → Upload binary → Stop VPS3/VPS2/VPS1 → Replace → Start VPS1/VPS2/VPS3 → Health check

## Hotfix Process

1. Branch from `main`
2. Fix + test
3. PR with `fix(scope):` commit message
4. Merge triggers auto-deploy
