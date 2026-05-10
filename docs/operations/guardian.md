# Validator restart authority — external supervisor model

## Summary

The Sentrix validator runtime does **not** ship an in-process watchdog
that can kill the process on liveness stalls. Restart authority lives
**outside** the validator process, in an external supervisor:

- `systemd` with `Restart=always` on the production validator units, or
- `docker` with `restart: unless-stopped` on the testnet docker stack, or
- a dedicated `sentrix-guardian` daemon for richer policy

This matches the pattern used by Tendermint / CometBFT, Geth, and
Cosmos SDK app chains. The runtime's job is to expose health metrics;
the supervisor's job is to decide when to restart.

## What the runtime exposes

The chain head, peer count, channel-drop counters, and similar gauges
are available through `/sentrix_status_extended`. An external monitor
polls that endpoint and decides when to act.

The most useful raw signals:

- `latest_height` — chain tip from this node's view.
- `peer_count` — verified libp2p peers.
- `EVENT_TX_DROPPED`, `BFT_TX_DROPPED`, `DROPPED_BFT_BROADCASTS` —
  cumulative back-pressure drops at the network/BFT boundary.
- `INBOUND_SILENCE_DISCONNECTS` — peers force-disconnected by the
  inbound-silence detector (libp2p peer management, not self-kill).
- `SWARM_TICK` — incremented every iteration of the libp2p select!
  loop. An external supervisor can compute "tick age" by subtracting
  successive values and comparing against wall-clock.

## What used to be in the runtime, and why it isn't any more

There were three in-process watchdogs at one point:

- libp2p swarm-task watchdog — fired `process::abort()` if SWARM_TICK
  stayed still for ~30 s.
- Validator-loop heartbeat watchdog — same, but on a per-iteration
  counter inside the validator main loop.
- Chain-height watchdog — same, but on `bc.height()`.

Operationally they worked: each kill cycle let `systemd Restart=always`
bring the process back, the libp2p mesh re-handshook, and the chain
broke out of whatever stuck state it was in. They were added during
real incidents and stopped real halts.

But two things were wrong with them:

1. They short-cycled the process every time consensus had a bad
   minute. That hid the underlying bugs — operators kept seeing
   "validator restarted, chain is fine" instead of seeing the actual
   nil-cascade or libp2p mesh stall in production logs. The fix
   was to remove the kill so the bugs become visible (PR #559 →
   PR #561).
2. No production blockchain ships in-process self-kill. The right
   tool for "process is unhealthy, please restart" is the supervisor
   layer (systemd, docker, k8s, sentrix-guardian). Putting it in the
   chain code is mixing concerns.

The 6 h `SENTRIX_SWARM_WATCHDOG_MODE=warn` testnet bake on 2026-05-10
proved the analysis: with kills disabled, restart count dropped from
74 / 6 h to 0, no safety regression, but the BFT round-cascade pattern
became immediately visible. That cascade was then fixed properly in
the engine (PR #561 Patch B — `catch_up_round` no longer eager-nil-votes).

After that, the watchdogs themselves were removed (this commit).

## Recommended supervisor policy

These are starting thresholds. Tune for your deployment.

| Severity   | Condition                                                      | Action                                  |
| ---------- | -------------------------------------------------------------- | --------------------------------------- |
| `warn`     | `latest_height` unchanged for > 120 s                          | Page on-call, collect logs.             |
| `critical` | `latest_height` unchanged for > 300 s                          | Collect logs, then restart the unit.    |
| `critical` | `EVENT_TX_DROPPED` increases by >1000 in 5 min                 | Collect logs, then restart the unit.    |
| `critical` | `peer_count` drops to 0 and stays there > 60 s                 | Collect logs, then restart the unit.    |

**Always collect logs before restart.** Auto-restart loops without log
capture is the failure mode that produced the original problem.

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

## What stays in the runtime

The runtime still does panic-on-bug — a Rust panic in any tokio task
hits a process-level panic hook that prints the panic + backtrace and
aborts. That is **not** a watchdog. It signals corrupted state /
broken invariant, the same way `panic!` works in Tendermint or any
other Rust/Go chain. The supervisor cycles the process the same way
it would for any other crash.

Genesis-init failure (e.g. premine credit overflow on cold-start)
still calls `process::exit(1)`. That is also not a watchdog — it's
"can't even reach a chain that's possible to run, stop now". The
supervisor will retry on its own schedule.

## What guardian is **not**

- Not a consensus participant. Guardian only restarts processes; it
  never touches BFT state, votes, or chain.db.
- Not a fork resolver. Forks are handled by the BFT engine + STATE-FP
  trace + chain.db rsync from canonical, all upstream of guardian.
- Not a substitute for fixing real BFT/libp2p liveness issues.
  Guardian's job is to make sure the runtime stays running; making
  the runtime *stop needing to be restarted* is engineering work.
