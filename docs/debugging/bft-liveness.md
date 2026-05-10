# Diagnosing BFT liveness stalls

## What "liveness stall" looks like

A Sentrix validator stuck in a BFT liveness stall has these symptoms:

- `bc.height()` is unchanged for minutes.
- Logs show BFT round numbers climbing: `round=0` → `round=1` → … →
  `round=10+`.
- `BFT skip round` and `BFT timeout — advanced to round N` messages
  fire repeatedly.
- Many round transitions end with a precommit nil-majority:
  `BFT #1d: precommit nil-majority skip … precommit_tally=[nil=4500B]`.
- Chain may eventually resume (round cascade resolves) or stay stuck
  until an external restart.

This is **not** a halt-class safety violation — no double-finalize,
no equivocation. It is a liveness issue: the engine cannot reach
quorum on a block.

## What the engine does at each round

For a 4-validator chain (3 / 4 stake quorum):

1. Proposer for `(height, round)` builds a block and gossips a
   `Proposal` over libp2p.
2. Each validator receives the proposal, validates, and casts a signed
   prevote for the proposed hash (or nil if locked on a different hash
   or the proposal didn't arrive).
3. Once a validator sees a 3 / 4 stake-weighted prevote supermajority
   for a non-nil hash, it casts a precommit for that hash.
4. Once a validator sees a 3 / 4 stake-weighted precommit
   supermajority, it finalizes the block.
5. If the round timer fires before quorum, the engine advances to the
   next round (skip-round). If a 3 / 4 stake-weighted nil precommit
   majority forms, same thing.

## Why nil-majority cascades happen

The dominant cause observed on testnet 2026-05-10 was libp2p **proposal
delivery jitter**:

- Round timeouts at the validator side fire ~13 s after the round
  starts.
- On a healthy mesh the proposal reaches all 4 validators within a
  few tens of milliseconds.
- Under jitter (gossipsub mesh stuck on a slow peer, libp2p select!
  loop briefly stalled, OS scheduler latency), the proposal arrives
  at some validators only after they have already prevoted nil on
  timeout.
- Once 2 / 4 validators have prevoted nil, the round can no longer
  reach 3 / 4 prevote supermajority for the block — even if the
  remaining 2 validators successfully receive the proposal and prevote
  for it.
- The engine then sees a nil-precommit majority and skips to the next
  round. Same proposer pattern, same race, same outcome — a cascade
  of nil rounds at one height while libp2p mesh is degraded.

There are other failure modes (locked-without-bytes livelock, real
network partition, real fork) but the common observed mode at
~16 s/blk testnet baseline is jitter-driven nil cascade.

## How to confirm it's liveness, not safety

Before declaring "this is a liveness issue", rule out:

```bash
# Any divergent state across validators at the same height?
journalctl -u sentrix-node --since "10 min ago" | grep STATE-FP | tail -30
# Any double-finalize / equivocation events?
docker logs --since 10m sentrix-testnet-val1 \
  | grep -E 'invalid previous|equivocation|double.sign' \
  | head -10
```

If both are clean (and `STATE-FP` fingerprints agree across hosts at
the same heights), it is a liveness issue.

## Logs to collect at a stall

```bash
# Per-validator BFT activity over the stall window.
docker logs --since 10m sentrix-testnet-val1 \
  | grep -E 'BFT (round|timeout|skip|finalized|prevote|precommit|proposal)' \
  | tail -100
# Round-skip and nil-majority counts.
docker logs --since 10m sentrix-testnet-val1 \
  | grep -cE 'BFT skip round|nil-majority'
docker logs --since 10m sentrix-testnet-val1 \
  | grep -cE 'BFT timeout'
# libp2p connection events (handshake errors, dropped peers).
docker logs --since 10m sentrix-testnet-val1 \
  | grep -E 'Handshake failed|outgoing connection error|outbound failure|Connection reset' \
  | head -20
# Health flags — confirm runtime is in degraded state.
curl -s http://127.0.0.1:9545/sentrix_status_extended | jq '.health'
```

## What `bft_liveness_degraded=true` means at runtime

The validator runtime sets this flag when either:

- the validator-loop heartbeat counter has not advanced for
  `HEARTBEAT_STALL_THRESHOLD` (60 s by default), or
- `bc.height()` has not advanced for `height_stall_threshold` (90 s
  by default; tunable via `HEIGHT_STALL_THRESHOLD_SEC`).

The flag is **cleared automatically** as soon as either side resumes
progress. An external supervisor should treat the flag as a
"page-the-operator" signal, not as a "kill the process now" signal.

A degraded flag for 30 s during a libp2p mesh hiccup is normal under
the current testnet conditions. A degraded flag held for > 5 min is a
real incident — collect logs (above), then restart the unit.

## Distinguishing the failure modes

| Pattern                                                              | Likely cause                              | Action                                         |
| -------------------------------------------------------------------- | ----------------------------------------- | ---------------------------------------------- |
| Round 0 → 1 → 2 → … → 10+; nil-majority each time; no peer errors    | libp2p proposal delivery jitter           | Wait — usually self-recovers in 1–3 min.       |
| Round 0 → 1 → 2 + many `outgoing connection error`                   | libp2p mesh partial partition             | Inspect peer connectivity; restart if stuck.   |
| Locked-on-different-hash + persistent nil prevote                    | Locked-without-bytes livelock             | Stale-lock relax should clear after gap; restart if not. |
| `STATE-FP` divergence at same height                                 | Real consensus / safety bug               | Halt cluster + STATE-FP RCA. Do NOT just restart. |
| `swarm_stalled=true` for >2 min, no log activity in swarm            | libp2p select! loop wedged                | Restart the unit (warn mode logs the stall but does not kill). |

## Reference: relevant code paths

- `crates/sentrix-bft/src/engine.rs::accept_proposal` — proposal-arrival
  prevote logic and lock-conflict nil-vote.
- `crates/sentrix-bft/src/engine.rs::on_prevote_weighted` — prevote
  supermajority detection and lock acquisition.
- `crates/sentrix-bft/src/engine.rs::on_timeout` — round-skip on timer.
- `crates/sentrix-network/src/libp2p_node.rs::SwarmWatchdogAction` —
  swarm-stall policy (default warn-only).
- `bin/sentrix/src/main.rs` validator-loop watchdog block — heartbeat
  and chain-height stall detection (warn-only since 2026-05-10).
