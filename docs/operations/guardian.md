# Validator restart authority — guardian model

## Summary

The Sentrix validator runtime does **not** call `process::abort()` or
`process::exit()` to recover from liveness stalls. Restart authority
lives **outside** the validator process, in an external supervisor:

- `systemd` with `Restart=always` on the production validator units, or
- `docker` with `restart: unless-stopped` on the testnet docker stack, or
- a dedicated `sentrix-guardian` daemon for richer policy

The runtime emits health flags and counters; the supervisor reads them
and decides whether to restart.

## Why this changed (2026-05-10)

Before this change, three watchdogs inside the validator process called
`std::process::abort()` on stall:

- **Swarm-task watchdog** (libp2p select-loop progress) — 30 s threshold
- **Validator-loop heartbeat watchdog** — 60 s threshold
- **Chain-height watchdog** — 90 s default threshold (env-tunable)

The 6 h testnet bake on 2026-05-10 demonstrated that these aborts were
**masking** a deeper BFT/libp2p round-cascade liveness issue rather than
fixing it. Each abort cycle reset the libp2p mesh, which incidentally
broke validators out of nil-precommit cascades — but it produced false
confidence: the underlying bug was never visible in production logs
because the kills always cleared it before it persisted.

When the kills were disabled (`SENTRIX_SWARM_WATCHDOG_MODE=warn`):

- Restart count dropped from 74 / 6 h → 0
- BFT round cascades became visible (round 5, 8, 11, 18+)
- Block-time degraded from 0.82 s/blk avg → 2.83 s/blk avg
- 0 safety regressions (no double-finalize, no equivocation, no
  invalid-prev)

The conclusion: in-process self-kill is the **wrong tool** for
recovering from a consensus-layer liveness issue. It hides the bug and
makes it harder to diagnose. The right tool is a metric and an
external supervisor.

## What the runtime exposes

### Counters (cumulative, never decrement)

| Metric                             | Source                          | Meaning                                                |
| ---------------------------------- | ------------------------------- | ------------------------------------------------------ |
| `sentrix_swarm_stall_total`        | `sentrix-network::libp2p_node`  | Each `STALL_THRESHOLD` (30 s) window with no progress. |
| `sentrix_heartbeat_stall_total`    | `bin/sentrix::main`             | Each `HEARTBEAT_STALL_THRESHOLD` (60 s) window.        |
| `sentrix_bft_height_stall_total`   | `bin/sentrix::main`             | Each `height_stall_threshold` (90 s default) window.   |

### Gauges (current state)

| Metric                                | Meaning                                                              |
| ------------------------------------- | -------------------------------------------------------------------- |
| `sentrix_swarm_last_tick_age_seconds` | Seconds since the libp2p select! loop last advanced.                 |
| `sentrix_validator_heartbeat_age`     | Seconds since the validator loop heartbeat last advanced.            |
| `sentrix_bft_height_stall_seconds`    | Seconds since `bc.height()` last advanced.                           |

### Health flags (boolean)

| Flag                       | Meaning                                                                      |
| -------------------------- | ---------------------------------------------------------------------------- |
| `swarm_stalled`            | `true` while the swarm task has been stuck > `STALL_THRESHOLD`.              |
| `bft_liveness_degraded`    | `true` while heartbeat or height stall is currently active.                  |

Flags are **automatically cleared** when the underlying counter advances
again — no operator intervention needed to reset them. They are exposed
via the `/sentrix_status_extended` RPC endpoint.

## Recommended supervisor policy

These are starting thresholds. Tune for your deployment.

| Severity   | Condition                                                              | Action                                       |
| ---------- | ---------------------------------------------------------------------- | -------------------------------------------- |
| `warn`     | `bft_height_stall_seconds > 120`                                       | Page on-call, collect logs.                  |
| `critical` | `bft_height_stall_seconds > 300` AND BFT round > 10                    | Collect logs, then restart the unit.         |
| `critical` | `swarm_stalled` is true AND `swarm_stall_total` increased twice in 5 m | Collect logs, then restart the unit.         |

**Always collect logs before restart.** The whole point of removing
the in-process kill was to keep the failure mode observable. A blind
auto-restart loses the diagnostic value.

A minimal log-collection step:

```bash
# Capture the last 5 minutes of validator + system logs before bouncing.
journalctl -u sentrix-node --since "5 min ago" > /tmp/stall-evidence-$(date +%s).log
```

For docker stacks:

```bash
docker logs --since 5m sentrix-testnet-val1 \
  > /tmp/val1-stall-$(date +%s).log
```

Restart only after the log is on disk.

## Opting back in to in-process kill (not recommended)

If a deployment really needs the historical kill behaviour (single-host
homelab, no external supervisor available), set:

```
SENTRIX_SWARM_WATCHDOG_MODE=abort
```

This restores `std::process::abort()` on the libp2p stall path. The
heartbeat and chain-height watchdogs in the validator runtime no longer
have an env-flag for kill mode — they are warn-only by design. If you
want them to terminate the process, your external supervisor should
enforce that based on the published health flags.

## What guardian is **not**

- Not a consensus participant. Guardian only restarts processes; it
  never touches BFT state, votes, or chain.db.
- Not a fork resolver. Forks are handled by the BFT engine + state-fp
  trace + chain.db rsync from canonical, all upstream of guardian.
- Not a substitute for fixing the underlying BFT/libp2p liveness
  issues. Guardian is a band-aid that makes operations sustainable
  while the deeper fixes are in flight.
