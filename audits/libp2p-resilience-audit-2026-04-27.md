# libp2p Resilience Audit — 2026-04-27

**Scope:** `crates/sentrix-network/src/libp2p_node.rs`
**Triggered by:** mainnet jail-induction test 2026-04-27 — stopping 1 of 4 validators caused 5+ minute chain stall, not recovered until rsync recovery

---

## verified_peers lifecycle

**Add path** (handshake completion):
- Line 1164 (inbound handshake) — after our peer sends Handshake, we verify chain_id + height matches
- Line 1611 (outbound handshake reply) — after we receive their Handshake response, we insert

**Remove path** (line 768-769 in ConnectionClosed handler):
```rust
if num_established == 0 {
    verified_peers.remove(&peer_id);
    let _ = event_tx.send(NodeEvent::PeerDisconnected(peer_id.to_string())).await;
}
```

This is **correct in theory** — only remove when all connections to that peer are gone. Bidirectional dialing creates 2 connections per peer pair; libp2p prunes duplicates. The `num_established == 0` guard prevents orphaning the surviving connection.

**Observation:** This lifecycle is sound. Removing 1 peer should NOT cascade to remove others. So why did peer_count hit 0 during testing?

---

## Hypothesis: brief peer_count=0 during peer-stop event

**What we observed (mainnet test, 2026-04-27 ~6:25 WIB):**
- Stopped Beacon validator at T=0
- Other 3 validators: peer_count expected 2 (Foundation+Treasury+Core seeing each other minus Beacon)
- Logs showed nil-precommit for h=690613 across ~10 rounds (5 minutes)
- Eventually chain stalled despite 3 online validators

**Possible causes (all need deeper investigation):**

1. **libp2p kademlia query churn** — When Beacon disconnect, kademlia may issue node-lookup queries to refresh routing table. These queries hold connections briefly. If the lookup fails or times out, connection state may flap.

2. **Connection-limit eviction race** — `MAX_LIBP2P_PEERS` cap (line 1127) rejects new connections if at limit. If kademlia opens transient connections + hits limit, real peer connections may be evicted.

3. **Identify protocol re-runs** — When peer disconnects, identify may run on remaining peers, briefly setting them to "unverified" state. peer_count is `verified_peers.len()` so unverified = 0.

4. **request_response queue drainage** — When validator A sends BFT proposal to peer B (offline), the request_response queue on A may hold onto pending sends. If the queue fills, may block sends to OTHER peers.

5. **Gossipsub mesh re-formation** — When peer leaves gossipsub mesh, `MeshConsumed`/`MeshLow` events fire. Mesh may temporarily exclude other peers during re-formation.

---

## Specific quick-win investigations (next session)

### Investigation A: instrument peer_count timing

Add tracing log on every `verified_peers.insert/remove` with timestamp. Correlate with BFT skip-round events to see if peer_count drops below 2 transiently when 1 of 4 validators offline.

```rust
// Before insert/remove:
tracing::info!(
    target: "libp2p::peers",
    "verified_peers: {} → {} ({})",
    before_count, after_count,
    if added { "add" } else { "remove" }
);
```

Effort: 1 hour. Output: definitive evidence whether peer_count drops to 0 during peer-leave event.

### Investigation B: dump all BFT request_response delivery failures

When BFT proposal/prevote/precommit fails to deliver via request_response, log it with target peer. Currently failures are silent (return Ok-ish at outer layer).

```rust
// In on_rr_event response handler:
if let Some(failure) = response.failure {
    tracing::warn!(
        target: "bft::network",
        "BFT message {kind} failed to deliver to {peer}: {failure}"
    );
}
```

Effort: 1-2 hours. Output: which validators saw which deliveries fail. May reveal whether stall is "votes never sent" vs "votes lost in transit".

### Investigation C: kademlia bootstrap interaction

Check if `KadBootstrap` (random walk) is triggered by ConnectionClosed events and if so, whether it disrupts existing connections.

Effort: 0.5-1 day code reading + testing.

---

## Quick-win fixes (low risk, defer-friendly)

### Fix 1: Aggressive reconnect on peer-down

When peer goes down, immediately schedule a reconnect attempt rather than waiting for kad routing table to age out. Current `reconnect_peers` (line 262) is manual-trigger only.

**Pseudocode:**
```rust
// In ConnectionClosed handler when num_established == 0:
if active_set.contains(&peer_id_to_addr(&peer_id)) {
    // This peer is in our known active set — try to reconnect
    schedule_reconnect_attempt(peer_id, delay = 5s);
}
```

Effort: 4-6 hours. Risk: none if delay > 0 and rate-limited.

### Fix 2: Tune libp2p connection-keepalive

Current keepalive may be too aggressive (kills idle connections). For BFT validators that may have <1 message/second between rounds, keepalive should be longer.

Effort: 1-2 hours code reading + tuning.

### Fix 3: Dedicated BFT request_response retry

Currently BFT messages have a built-in rebroadcast (#1d). But that's for proposer-side. Vote messages from non-proposers don't have retry logic — if delivery fails, vote is lost.

**Proposal:** add `BftMessageQueue` that retries up to N times on delivery failure for prevote/precommit messages.

Effort: 1-2 days. Potentially significant impact.

---

## Why this stayed broken

Chain has been operational with this behavior for weeks. The asymmetric recording bug (PR #356) MASKED some symptoms because validators eventually agreed via block re-broadcast. With asymmetric recording fixed, partial-mesh scenarios are more likely to expose libp2p issues that were previously hidden.

In other words: today's PR #356 made jail-divergence less likely, but exposed deeper libp2p resilience gaps that were always there.

---

## Recommendation

Item 2 from production-readiness audit (libp2p resilience) requires:
- 0.5-1 day investigation A + B (instrumentation)
- Run a fresh-brain test with that instrumentation
- Identify which sub-cause is actually firing
- Then apply targeted quick-win fix (3-5 days total)

Until then: **avoid stopping individual validators on mainnet.** Use halt-all + simultaneous-start for any operational change. This is the same advice from `feedback_mainnet_restart_cascade_jailing.md` memory + RECENTLY confirmed validity in 2026-04-27 mainnet test.
