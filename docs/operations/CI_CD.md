# CI/CD

GitHub Actions pipeline. Every merge to `main` runs tests, builds a binary, and deploys to all production nodes.

## Pipeline

```
Push to main → TEST → DEPLOY (only main branch)
```

### Test Job (every push + PR)

1. `cargo deny check` — license + supply chain
2. `cargo clippy -- -D warnings` — zero warnings (deny unwrap/expect/panic)
3. `cargo build --release`
4. `cargo test` — 357 tests
5. Upload binary as artifact (1-day retention)

### Deploy Job (main only, after test passes)

Phase 1 — Upload binaries to all VPS while nodes still running (minimize downtime).

Phase 2 — Stop in reverse order:
```
[NODE_3] → [NODE_2] → [NODE_1]
```
Secondary nodes stop first so primary can finish processing in-flight blocks. Prevents orphan blocks.

Phase 3 — Replace binary on each VPS:
```bash
cp /tmp/sentrix-new /opt/sentrix/sentrix && chmod +x /opt/sentrix/sentrix
```

Phase 4 — Start in forward order:
```
[NODE_1] → [NODE_2] → [NODE_3]
```
Primary first so peers can connect immediately.

Phase 5 — Health check after 35s stabilization:
```bash
curl -sf http://[NODE_IP]:8545/chain/info | jq .height
```
Verify all nodes responding and height advancing.

## Branch Protection

`main` requires PR + CI pass. Admin can bypass in emergencies.

## Design Choices

Binary artifacts, not Docker. All nodes get the exact same compiled binary. No registry dependency.

Stop/start order matters. Learned from production — wrong order causes chain forks.

No auto-rollback. If deploy fails health check, investigate manually. Auto-rollback masks root causes.
