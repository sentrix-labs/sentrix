---
sidebar_position: 18
title: Mainnet Observability
---

# Mainnet observability

Three-tier monitoring stack on the build host that catches mainnet stalls + per-validator divergence + auto-recovers. Shipped 2026-04-28 after a 30-min h=773013 stall ran undetected because alerting was incomplete.

## Stack overview

| Tier | Component | Cadence | What it watches |
|---|---|---|---|
| 1 | **Watchdog daemon** (`scripts/watch-mainnet.sh`) | every 30s | Edge stall + per-validator height lag + RPC reachability |
| 2 | **Prometheus + Alertmanager** | 15s scrape | 12 alert rules across chain + hosts + info |
| 3 | **External uptime check** (UptimeRobot, recommended) | 5 min | HTTP keyword on `/sentrix_status` from outside the fleet |

All three converge on the same Telegram bot (`@Sentrixnotif_bot` via Alertmanager Telegram receiver). Multi-source redundancy means failure of one path doesn't blind the operator.

## Tier 1 — Watchdog daemon

`scripts/watch-mainnet.sh` runs every 30 seconds via `sentrix-watchdog.timer` systemd unit. Three checks per tick:

### Check 1: Edge stall

Polls `https://rpc.sentrixchain.com/sentrix_status` for `latest_block_height`. If height doesn't advance in 2 minutes → WARN, in 5 minutes → CRITICAL with optional auto-recovery.

### Check 2: Per-validator lag

Probes each validator's `:8545` (or `:8549` for the Treasury validator on its non-default port) and compares against the cluster median. Any validator > 10 blocks behind → WARN. Catches the divergence pattern that caused the 2026-04-28 stall (one validator silently fell 1 block behind, BFT then couldn't recover for 30 min).

### Check 3: RPC reachability

Times out on per-validator probe → WARN. Catches the case where systemd reports a service "active" but the validator process is hung (RPC port not responding).

### Alert routing

Configured in `/etc/sentrix/watchdog.env`:

```bash
TELEGRAM_BOT_TOKEN=<from BotFather>
TELEGRAM_CHAT_ID=<your operator chat>
DISCORD_WEBHOOK_URL=                # optional, parallel route
AUTO_RECOVERY=false                  # flip true after 1-2 weeks soak
STALL_WARN_SEC=120
STALL_CRITICAL_SEC=300
LAG_WARN_BLOCKS=10
```

### Auto-recovery

When `AUTO_RECOVERY=true`, watchdog runs `scripts/recover-mainnet.sh` automatically after 5 min of stall. Recovery sequence:

1. Probe per-validator height; identify canonical (highest with ≥ 2 agreeing peers).
2. If any lagger > 5 blocks behind canonical → halt, backup divergent chain.db (timestamped), tar-pipe canonical chain.db over.
3. halt-all + simul-start in parallel across all 4 validators.
4. Verify chain advances within 25s; exit code 2 if still stuck (operator escalation needed).

Proven 2026-04-28: end-to-end recovery in **36 seconds** (vs the 30 min manual MTTR observed earlier same day). Forensic backups of divergent chain.db are preserved at `/opt/<service>/data/chain.db.divergent-h<H>-<timestamp>/`.

## Tier 2 — Prometheus + Alertmanager

[Prometheus](https://prometheus.io/) on the build host scrapes 10 targets every 15-30s:

| Job | Targets | Purpose |
|---|---|---|
| `node_exporter` | All 5 VPS at `:9100` | OS metrics (CPU, RAM, disk, network) |
| `sentrix-mainnet` | 4 mainnet validators at `:8545` (vps2: `:8549`) | Chain metrics (height, mempool, fees, validators) |
| `prometheus` | self at `localhost:9090` | self-scrape |

### Alert rules (12 total)

**Chain (6):**

- `ChainHeightStalled` (critical) — `delta(sentrix_block_height[2m]) == 0`, fires after 1m
- `BlockTimeDegraded` (warning) — `sentrix_block_time_seconds > 8`, fires after 2m
- `NoActiveValidators` (critical) — `sentrix_active_validators == 0`, fires after 30s
- `PeerBlockSaveFailing` (critical) — `rate(sentrix_peer_block_save_fails_total[5m]) > 0`
- **`ValidatorLagBehindCluster`** (warning) — per-validator height vs cluster max > 10, fires after 2m. Catches the divergence pattern that today's halt trained on.
- **`ValidatorHeightSpread`** (critical) — cluster max - min > 20, fires after 2m. Catches split-brain even when no single vps is "the lagger".

**Hosts (3):**

- `TargetDown` (warning) — any scrape target unreachable for >3m
- `DiskSpaceLow` (warning) — root mount < 15% free for >15m
- `HighMemoryUsage` (warning) — RAM > 90% for >10m

**Info (3):**

- `ValidatorSetChanged` (info) — admin op fired (audit trail)
- `MempoolHot` (info) — `sentrix_tx_pool_size > 100` for >2m
- `BlockHeightMilestone` (info) — every 100K blocks

### Telegram delivery

Alertmanager (port 9093) routes alerts to Telegram via the `@Sentrixnotif_bot` receiver:

```yaml
receivers:
  - name: telegram
    telegram_configs:
      - bot_token: <token>
        chat_id: <operator_chat_id>
        send_resolved: true
        parse_mode: HTML
```

Severity tiers: `critical` = group_wait 5s, repeat every 10m. `warning` = group_wait 15s, repeat every 30m. `info` = group_wait 1m (batch), repeat 720h (fire-once).

## Tier 3 — External uptime (UptimeRobot)

Eliminates the "what if the build host itself dies" gap. Free tier covers 50 monitors at 5-min interval. Setup at [uptimerobot.com](https://uptimerobot.com):

1. **Sign up** (free).
2. **Monitor 1 — Mainnet RPC alive** — type `HTTP(s) — Keyword`, URL `https://rpc.sentrixchain.com/sentrix_status`, keyword `latest_block_height`.
3. **Monitor 2 — Testnet RPC** — same with `testnet-rpc`.
4. **Monitor 3-5** — explorer + faucet + docs (HTTP-only, 200 check).
5. **Telegram integration** — Profile → Integrations → Add Telegram → use the same `@Sentrixnotif_bot`.

## Manual operator commands

```bash
# Tail live ticks
sudo journalctl -t sentrix-watchdog -f

# Force a tick now (debug)
sudo systemctl start sentrix-watchdog.service

# Read current state
cat /var/lib/sentrix-watchdog/state.json | jq

# Trigger manual recovery (skip waiting for stall threshold)
~/founder-private/scripts/recover-mainnet.sh

# Disable temporarily (planned maintenance)
sudo systemctl stop sentrix-watchdog.timer
```

## Known limitations

- **Build-host SPOF for tiers 1-2** — both run on the build host. If that host dies, only Tier 3 (UptimeRobot) pages. Mitigation: ensure UptimeRobot is configured before relying on the stack.
- **Heuristic canonical selection in `recover-mainnet.sh`** — picks "highest height with ≥ 2 agreeing peers". Edge case: 4-way split-brain (all 4 different heights) picks the highest. Adequate for the common 1-validator-lagging pattern but not for full consensus splits.
- **No state_root cross-check at same height** — detects HEIGHT lag, not state divergence at matching heights. Same-height divergence would slip through. The `state-divergence-recovery` runbook covers that case manually.
- **Auto-recovery may halt during legitimate slow blocks** — if real network conditions cause a 5-min slowdown without divergence, auto-recovery would unnecessarily halt-all. Soak with `AUTO_RECOVERY=false` long enough to trust the threshold before flipping on.

## See also

- [`runbooks/mainnet-watchdog.md`](https://github.com/satyakwok/sentrixfounder-private/blob/main/runbooks/mainnet-watchdog.md) — operator runbook (private)
- [Monitoring](./MONITORING) — Prometheus + Grafana setup overview
- [Emergency Rollback](./EMERGENCY_ROLLBACK) — when to restore from off-host backup
- [Testnet Recovery](./TESTNET_RECOVERY) — testnet-only recovery patterns
