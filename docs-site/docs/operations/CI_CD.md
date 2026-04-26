# CI/CD

GitHub Actions runs the **test** job on every push and PR. The
**deploy** job is **disabled** — production binaries ship via an
operator-run deploy from a build host, not from CI.

## Pipeline

```
Push / PR  →  TEST  →  (deploy disabled)
                       ↓
                  Operator deploy from build host (manual)
```

### Test Job (every push + PR)

1. `cargo deny check` — license + supply chain
2. `cargo clippy --tests -- -D warnings` — zero warnings (deny unwrap/expect/panic)
3. `cargo build --release`
4. `cargo test` — 551+ tests across the 14-crate workspace
5. Upload binary as artifact (1-day retention, audit-only)

The artifact is **not** auto-deployed; it exists so reviewers can pull
the exact CI binary if they need to reproduce a test result.

## Deploy

A build host (a dedicated dev/edge host with the cargo cache and SSH
keys) is the canonical deploy origin. The build runs inside a
`rust:1.95-bullseye` container (glibc 2.31, compatible with all current
target distros), the binary is uploaded to validators over a private
network (e.g. WireGuard mesh), and services are restarted in a rolling
pattern with a bounded health check.

**Third-party validators** can use the generic primitive at
`scripts/deploy-validator.sh`:

```bash
./scripts/deploy-validator.sh \
  --ssh-key  ~/.ssh/my_operator_key \
  --host     operator@validator.example.com \
  --service  sentrix-node \
  --bin-dir  /opt/sentrix \
  --rpc-url  http://127.0.0.1:8545 \
  --binary   ./target/release/sentrix
```

Wrap it in a per-fleet loop / Ansible play / k8s rollout for multi-validator
operations.

**Maintainer fleet** orchestration lives in the maintainer's private
operations repository. It bakes in maintainer-specific infrastructure
(WireGuard IPs, role-to-host mapping, SSH key paths) and is not
generally useful to operators running their own fleet.

### Rolling-restart guidance

The standard pattern: stop validators in reverse priority order, start
them in forward priority order. Primary validator finishes processing
in-flight blocks last and comes back up first so peers reconnect
quickly. Wrong order can produce orphan blocks.

### Health check

After ~35s stabilisation, poll `/chain/info` on each validator and
verify height is advancing. `scripts/deploy-validator.sh` does this
automatically.

## Branch Protection

`main` requires PR + CI test pass. Admin can bypass in emergencies.

## Design Choices

- **Binary artifacts, not Docker** for mainnet. All nodes get the exact
  same compiled binary from one build. No registry dependency.
  (Testnet runs in Docker on the build host since 2026-04-23 for fast iteration.)
- **CI test, not CI deploy.** Auto-deploy + manual hot-deploy creates a
  race where the same commit ships twice. Disabled in favour of a
  single canonical operator-driven path.
- **No auto-rollback.** If a deploy fails health check, investigate
  manually. Auto-rollback hides root causes.
