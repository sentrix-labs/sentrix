# CI/CD

GitHub Actions runs the **test** job on every push and PR. The
**deploy** job is **disabled** — production binaries ship via
`scripts/fast-deploy.sh` from build host, not from CI.

## Pipeline

```
Push / PR  →  TEST  →  (deploy disabled)
                       ↓
                  fast-deploy.sh from build host (manual)
```

### Test Job (every push + PR)

1. `cargo deny check` — license + supply chain
2. `cargo clippy --tests -- -D warnings` — zero warnings (deny unwrap/expect/panic)
3. `cargo build --release`
4. `cargo test` — 551+ tests across the 14-crate workspace
5. Upload binary as artifact (1-day retention, audit-only)

The artifact is **not** auto-deployed; it exists so reviewers can pull
the exact CI binary if they need to reproduce a test result.

## Deploy: `scripts/fast-deploy.sh` (from build host)

build host (the dev/edge host) is the canonical deploy origin. The script
builds inside a `rust:1.95-bullseye` container (glibc 2.31, compatible
with both Ubuntu 22.04 and 24.04 production targets), uploads the
binary to Foundation node/Treasury node/Core node over the wg1 WireGuard mesh, and does a
rolling restart with a bounded health check.

```bash
# From build host
./scripts/fast-deploy.sh mainnet          # asks for confirmation
./scripts/fast-deploy.sh testnet          # silent (testnet docker)

# Rollback to a prior release on disk
SENTRIX_ROLLBACK=/opt/sentrix/releases/<prev> \
  ./scripts/fast-deploy.sh mainnet
```

End-to-end ~3–5 minutes. Builds once, copies the same byte-identical
binary to every host (no per-host recompile, no glibc skew).

### Stop / start order (rolling)

The script stops validators in reverse order and starts them in forward
order:

```
stop:  Core node → Treasury node → Foundation node
start: Foundation node → Treasury node → Core node
```

Primary (Foundation node, Foundation) finishes processing in-flight blocks last and
is back up first so peers reconnect quickly. Wrong order can produce
orphan blocks — learned the hard way.

### Health check

After 35 s stabilization the script polls `/chain/info` on each VPS
and verifies height is advancing.

## Break-Glass: `scripts/emergency-deploy.sh`

Same primitive as `fast-deploy.sh` but **skips the preflight test gate**
and requires a strict confirmation phrase. Use only when:

- GitHub Actions is down and you need to ship a fix.
- The chain has halted and a regression must be bypassed.
- An exploit is in flight and the patch ships ahead of normal CI.

This is rare — `fast-deploy.sh` is the default path.

## Branch Protection

`main` requires PR + CI test pass. Admin can bypass in emergencies.

## Design Choices

- **Binary artifacts, not Docker** for mainnet. All nodes get the exact
  same compiled binary from the build host build. No registry dependency.
  (Testnet runs in docker on build host since 2026-04-23.)
- **CI test, not CI deploy.** Auto-deploy + manual hot-deploy creates a
  race where the same commit ships twice. Disabled in favour of a
  single canonical path.
- **No auto-rollback.** If a deploy fails health check, investigate
  manually. Auto-rollback hides root causes.
