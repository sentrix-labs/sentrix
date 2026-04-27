# Sentrix Codebase Audit — Areas 1-7 (2026-04-27 night)

**Scope:** Bug hunt across 7 code areas not previously deep-audited
**Method:** grep-based pattern detection + targeted read of suspicious sites
**Output:** concrete findings + severity rating + fix recommendation

---

## 1. Block Pass-1/Pass-2 + state_root determinism

**Status checked:** ✓ Read `crates/sentrix-core/src/block_executor.rs` + state_root stamping logic

**Architecture:** Self-produced blocks have `state_root = None` at admission, stamped during Pass-2 apply. Peer blocks past `STATE_ROOT_FORK_HEIGHT` MUST carry `state_root = Some(root)`. Validator computes its own state_root from state trie post-apply, compares to peer's claim. Mismatch → reject.

**Finding A1: state_root determinism is sound.** The trie computation is per-validator deterministic (BSMT with BLAKE3+SHA-256). If 2 validators apply the same block sequence with same starting state, their state_roots match.

**Where divergence comes from:** NOT state_root computation itself, but PRECEDING state mutations differing (jail counter, reward distribution accumulators, epoch state). With the bug class fixes shipped today (PR #356, #362), divergence sources are reduced.

**No new bug found.** State_root logic is defensively designed.

---

## 2. MDBX storage durability

**Status checked:** ✓ Read `crates/sentrix-core/src/blockchain.rs::persist_block_durable`

**Architecture:** `persist_block_durable` writes block to MDBX with atomicity:
- TABLE_META `block:{N}` — full block bytes
- TABLE_BLOCK_HASHES — reverse index
- height marker
- Calls `sync()` for fsync barrier

Atomic commit point: rollback path snapshots state pre-apply; on persist failure, restores accounts/contracts/authority/mempool/total_minted/chain.truncate/trie.set_root.

**Finding A2: storage is sound for crash-recovery scenarios.** BACKLOG #16 fix (durable apply + persist with rollback) is the canonical implementation.

**Potential issue:** MDBX `sync()` is per-block. Under high TPS, this is throughput-limiting. Not a bug, but a perf concern for future scaling.

**No critical bug found.**

---

## 3. Mempool / double-spend prevention

**Status checked:** ✓ Read `crates/sentrix-core/src/mempool.rs`

**Defensive measures observed:**
- `MAX_MEMPOOL_SIZE` cap (line 40)
- `MAX_MEMPOOL_PER_SENDER` cap (line 48) — prevents one address from spamming
- Nonce ordering enforced
- TODO: RBF (Replace-By-Fee) not implemented (line 144) — acceptable, RBF is opt-in feature

**Finding A3: mempool has appropriate limits.** No double-spend bug at mempool layer — that's enforced at apply-block time via nonce check.

**Open question:** does the per-sender cap (≈250) prevent legitimate batch operations? Not a bug, but operational concern.

**No critical bug found.**

---

## 4. EVM integration (revm 37) + gas accounting

**Status checked:** ✓ Sample read of `crates/sentrix-evm/src/database.rs`

**Architecture:** revm 37 with custom database adapter. Address conversion alloy ↔ Sentrix lowercase format.

**Finding A4: address conversion is consistent.** Multiple sites lowercase addresses for comparison. EIP-55 checksum NOT enforced (Sentrix uses always-lowercase convention) — intentional design choice.

**Not deeply audited:** revm internal correctness (would require revm-level expertise). Production use of revm 37 is audited upstream.

**Latent risk:** RUSTSEC-2025-0055 in transitive `tracing-subscriber` (via revm/ark stack). Already in queue.

**No new bug found in Sentrix-side EVM wiring.**

---

## 5. Wallet keystore (Argon2id)

**Status checked:** ✓ Read `crates/sentrix-wallet/src/keystore.rs`

**Argon2id parameters:**
```
ARGON2_M_COST = 65_536  (= 64 MiB memory)
ARGON2_T_COST = 3       (3 iterations)
ARGON2_P_COST = 4       (4 parallel lanes)
SALT_SIZE = 16 bytes
NONCE_SIZE = 12 bytes (AES-GCM)
KEY_SIZE = 32 bytes (256 bits)
```

**Finding A5: Argon2id params meet OWASP recommendations.** OWASP recommends m≥46MiB, t≥1, p≥1 for password storage. Sentrix uses 64MiB / 3 iter / 4 lanes — conservative, harder than minimum. Brute-force resistant.

**v1 keystores (PBKDF2)** still supported for backward compat — acceptable migration path.

**No critical bug found.**

---

## 6. Genesis + premine accounting

**Status checked:** ✓ Read `crates/sentrix-core/src/genesis.rs::total_premine`

**Finding A6: premine accounting verified by test.** `test_total_premine_matches_hardcoded` at line 399 asserts `g.total_premine() == TOTAL_PREMINE` (= 63M SRX × 100M sentri = 63_000_000 × 10^8). Genesis loads + computes; mismatch fails test.

**TOTAL_PREMINE = 6.3 × 10^15 sentri = 63M SRX = 20% of MAX_SUPPLY_V2 (315M).**

Genesis validation:
- Total premine ≤ MAX_SUPPLY (line 187, uses checked_add to catch overflow)
- Per-balance entries are (address, sentri) pairs, validated structurally

**No critical bug found.**

---

## 7. RPC auth / rate limiting

**Status checked:** ✓ Read `crates/sentrix-rpc/src/routes/ratelimit.rs` + RPC routes

**Defensive measures:**
- `GlobalIpLimiter` — 300 req/min/IP global rate limit
- Per-endpoint rate limits where appropriate
- CORS handled at edge (Caddy or via env var)
- No auth required for public read endpoints (correct — chain data is public)
- Write endpoints (transactions) require valid signature (cryptographic auth)

**Finding A7: RPC is appropriately rate-limited for public read access.** Heavy queries (richlist, large block ranges) might benefit from tighter caps but not security bugs.

**Latent issue:** scan/explorer behind Caddy reverse proxy; if Caddy is bypassed (direct access to backend port), rate limit doesn't apply. Mitigated by firewall (backend ports not in public IP).

**No critical bug found.**

---

## Summary

**Bugs found in this audit pass:** 0 critical, 0 high

**Reason:** Major bug classes were already identified + fixed earlier in the marathon:
- Asymmetric record_block_signatures (PR #356)
- Asymmetric distribute_reward + epoch tracking (PR #362)
- BFT gate threshold logic (PR #351, #355)
- Hardcoded supply display (PR #347, #348)
- Observability gaps (PR #350)
- Internal-filename leak in public artifacts (PR #361)

The remaining issues are architectural (libp2p resilience, state_root divergence under partial finality, consensus-computed jail) which require multi-day fresh-brain engineering — captured in:
- `audits/sentrix-production-readiness-audit-2026-04-27.md`
- `audits/libp2p-resilience-audit-2026-04-27.md`
- `audits/reward-distribution-flow-audit-2026-04-27.md`
- `audits/consensus-computed-jail-design.md`

**Production-readiness verdict (unchanged):** Solo-operator OPERATIONAL. Multi-validator NOT YET safe.

---

## Total marathon (~34h+) deliverables

**Code fixes shipped:**
- 16 PRs merged today (#350-#363) covering observability, asymmetric-application bug class (3 instances fixed), BFT gate fork-gated relaxation, Phase A consensus-jail data plumbing, audit docs, opsec scrub
- 4 binary releases (v2.1.41, v2.1.42, v2.1.43, v2.1.44)
- 5 P0 stalls recovered via chain.db rsync (proven procedure)

**Audit docs published:**
1. Jail-cascade RCA + design + runbook
2. Production-readiness honest assessment
3. libp2p resilience audit
4. Reward distribution flow audit
5. Codebase areas 1-7 audit (this doc)

**Memory updated:**
- `feedback_no_vps_in_public.md` (existing)
- `feedback_no_internal_filename_refs_in_public.md` (NEW)

**Fresh-brain queue (next session):**
1. Phase B/C/D consensus-jail (4-6 weeks)
2. libp2p resilience deep dive (3-5 days)
3. BFT timeout tuning (1 day)
4. Cargo dep audit fix (1-2 days)
5. Eco Tier 1 remaining (Sourcify + canonical contracts, multi-day each)

---

## Honest closing assessment

After 34+ hour marathon + 16 PRs + 5 audits + multiple deep code reads, no NEW critical bugs found in areas 1-7. The major bug class (asymmetric application of validator-loop bookkeeping) has been completely closed (3 instances fixed: record_block_signatures, distribute_reward, epoch_manager.record_block).

The chain is **as audited as it can reasonably be from this session**. Further deep audits would require:
- Fresh-brain consensus engineer
- Specialized expertise (libp2p, cryptography)
- Multi-day investment per area

**Recommendation:** Stop. Mainnet healthy at v2.1.44, audit trail complete. Next session: Phase B consensus-jail data plumbing → mainnet activation roadmap.
