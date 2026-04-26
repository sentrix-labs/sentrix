# Changelog

## [2.1.38] — 2026-04-26 — Legacy TCP-path deletion + cumulative skip metric

Hardening on top of v2.1.37 (same incident surface). PR #334 second + third commits.

### Removed
- `crates/sentrix-network/src/sync.rs` deleted entirely (158 LOC, dead code).
- `crates/sentrix-network/src/node.rs` trimmed 645 → 36 LOC. Kept only `NodeEvent`, `SharedBlockchain`, `DEFAULT_PORT`. Both deleted sites had the same `for block in batch` cascade-bail bug pattern as the v2.1.37 fix surface — eliminating dead code with a known bug rather than carrying defensive filters.

### Added
- `static SYNC_SKIPPED_TOTAL: AtomicU64` cumulative counter in `libp2p_node.rs`.
- Threshold-crossing WARN log at 10/100/1k/10k/100k cumulative skipped — surfaces re-emergence of the concurrent-GetBlocks race so operators can decide when to ship single-flight coalescing.

### Migration
- Drop-in chain.db compatible with v2.1.37.

---

## [2.1.37] — 2026-04-26 — libp2p sync cascade-bail fix (mainnet stall RCA)

P0 hotfix. Mainnet stalled at h=604547 for ~1h 45min on 2026-04-26 morning. PR #334 first commit.

### Root cause
`libp2p_node.rs` BlocksResponse handler bailed on the first already-applied block in a batch and dropped the rest of valid forward blocks in the same response. Concurrent GetBlocks paths (periodic `sync_interval` + `TriggerSync` + reactive chain-on-full-batch) all read `our_height` and ask `from: our_height+1`. Responses overlap. Cumulative drift over thousands of sync rounds → 4-way chain.db divergence at h=604547 across the 4 mainnet validators.

### Fixed
- `crates/sentrix-network/src/libp2p_node.rs` BlocksResponse loop: filter `block.index <= chain.height()` BEFORE `add_block_from_peer`. Skip duplicates silently, keep applying forward blocks. Loop only breaks on real validation errors.

### Tests
- `test_libp2p_sync_loop_skips_duplicates_and_applies_remaining` in `crates/sentrix-core/tests/fork_determinism.rs` — racy batch with already-applied prefix advances chain to expected height instead of stalling.

### Recovery (operator-driven)
1. Forensic backup divergent chain.db on each validator
2. Treasury picked as canonical (most progressed, self-consistent, signer-set matched majority)
3. Tar-pipe Treasury chain.db → Foundation, Core, Beacon
4. MD5 parity confirmed (`mdbx.dat` md5 = `567c7165...`)
5. v2.1.37 binary deployed (docker bullseye, glibc 2.31)
6. Rolling restart: Treasury → Foundation → Core → Beacon

### Migration
- Drop-in chain.db compatible with v2.1.36.

---

## [2.1.36] — 2026-04-26 — V4 reward v2 + 14 PR marathon

Single-night marathon (PRs #316–#331; binaries v2.1.31 → v2.1.36). All 4 mainnet validators on v2.1.36 in Voyager DPoS+BFT.

### Added
- **V4 reward distribution v2** active since h=590100. Coinbase 1 SRX/block routes to `PROTOCOL_TREASURY` (`0x0000…0002`) escrow; validators + delegators claim via `StakingOp::ClaimRewards` (no-amount staking op with apply-time treasury credit). Stake-weighted delegator share supported, slashing applies to `pending_rewards` before claim.
- **`tools/claim-rewards/`** — standalone Cargo binary. Reads 64-hex privkey from stdin, queries pending via `/staking/validators`, builds + submits ClaimRewards tx. Proven end-to-end on Core validator.
- **`docs/operations/CLAIM_REWARDS.md`** — operator guide (mechanism diagrams, query/submit/verify procedure, failure modes).
- **`/staking/validators`** JSON-RPC field `pending_rewards` per validator.
- **`SwarmCommand::GetConnectedPeers`** + `LibP2pHandle::connected_peers()` — used by L1 dial-tick to skip already-connected peers.
- **`connection_limits::Behaviour`** in libp2p swarm (`max_established_per_peer = 2`, env-overridable). Caps connection accumulation between validator pairs.
- **L1 multiaddr `/p2p/<peer_id>` suffix** — appended to validator advert multiaddrs so receiving validators can extract `peer_id` for `dial_known()` short-circuit.
- **Frontier Phase F-2 shadow-mode wiring** (`SENTRIX_FRONTIER_F2_SHADOW=1`) — `build_batches` observer in `apply_block_pass2`, read-only.
- **BFT signing v2 Phase 1 foundation** — `signing_payload_for_height(...)` dispatch helper + low-S enforcement scaffold (Phase 2 call-site refactor pending dedicated session).
- **`docs/roadmap/PHASE3.md`** — Frontier roadmap (F-1✅, F-2✅, F-3 → F-10 pending; ~6-8 weeks calendar).

### Changed
- **PHASE2.md rewritten** from "Planned" to "ACTIVE on mainnet since 2026-04-25" — three pillars (DPoS + BFT + EVM), V4 reward subsystem, network hardening summary.
- **Voyager dispatch is now runtime-aware** — `Blockchain::voyager_mode_for(&self, height)` ORs env `VOYAGER_FORK_HEIGHT` with chain.db `voyager_activated`. Production callsites in `block_executor.rs` (validate_block + EVM tx check) + `jsonrpc/sentrix.rs` (getValidatorSet + getFinalizedHeight) migrated. Closes the env-var-default-`u64::MAX` foot-gun that caused validate_block to fall into Pioneer auth post-restart.
- **Mempool tx validation** exempts staking ops from `amount > 0` check (alongside existing token-op + EVM-tx exemptions). Allows ClaimRewards (`amount=0`, `to=PROTOCOL_TREASURY`).
- **Bootstrap-peers symmetric on all 4 systemd units** — each validator now lists all 3 others in `--peers`. Post-restart mesh self-heals without parallel-restart recovery.
- WHITEPAPER bumped to v3.3 with new §2.7 V4 Reward Distribution section.
- Public docs synced to v2.1.36 (README, NETWORKS, EMERGENCY_ROLLBACK).

### Fixed
- **VOYAGER_FORK_HEIGHT env default `u64::MAX` bug** — caused h=597524 stall (`is_voyager_height()` returned false → validate_block fell into Pioneer auth → rejected Voyager skip-round blocks as "Unauthorized validator"). Fixed via PR #324 (`voyager_mode_for` runtime check) + operator hot-fix (env=579047). Incident report: `incidents/2026-04-26-voyager-fork-height-env-bug.md` (founder-private).
- **libp2p connection accumulation** — 4-tier hardening: dial-tick connected-peers pre-check (#319) + advert `/p2p/` suffix (#321) + connection_limits cap (#323) + max-per-peer 1→2 hotfix (#326).
- **`fast-deploy.sh REPO_ROOT`** broken after script moved to founder-private — added `SENTRIX_REPO` env-var override.
- **`cp ETXTBSY`** when overwriting running executable — switched to cp-to-tmp-stage-then-mv-rename pattern.

### Notes
- `is_voyager_height()` static check kept for test compatibility (production callsites all migrated to `voyager_mode_for()`).
- BFT signing v2 Phase 2 (~31 mechanical call-site changes) deferred to dedicated fresh-brain session per consensus discipline.

---

## [2.1.30] — 2026-04-25 — Voyager DPoS+BFT + EVM activation

Pivotal release: mainnet hard-fork from Pioneer (PoA round-robin) to Voyager (DPoS+BFT) at h=579047. EVM (revm 37) activated in same window.

### Added
- **Voyager DPoS+BFT consensus** active on mainnet since h=579047. 4 validators (Foundation, Treasury, Core, Beacon), stake-weighted active set, 28800-block epochs, 3-phase Tendermint-style BFT (propose / prevote / precommit) with skip-round proposer rotation. `BlockJustification` carries supermajority precommits.
- **EVM** active since h=579060 (`evm_activated=true`). MetaMask compatibility (`eth_sendRawTransaction`, `eth_call`, `eth_getBalance`, etc); 0.1 sentri/gas, 30M block gas limit, chain ID 7119/7120.
- **L1 multiaddr advertisements** — `sentrix/validator-adverts/1` gossipsub topic; signed `MultiaddrAdvertisement` broadcasts every 10 min; auto-dial. Self-healing mesh from a single bootstrap peer.
- **L2 cold-start gate** — validator loop refuses BFT entry until `peer_count >= active_set.len() - 1`. Closes the activation #1 livelock cause.
- **`/sentrix_status` + `/chain/info`** consensus reporting fields (`consensus_mode`, `voyager_activated`, `evm_activated`).
- **Frontier Phase F-1 type scaffold** (`AccountKey`, `TxAccess`, `Batch`, `derive_access`, `build_batches` stubs).

### Changed
- WHITEPAPER bumped to v3.2 with new §2.5 Voyager DPoS+BFT design + §2.6 L1/L2 peer auto-discovery sections.
- `SENTRIX_FORCE_PIONEER_MODE` removed from all env files.
- CLAUDE.md trimmed to ~25 incident-earned rules.

### Fixed
- Issue #268 (legacy block tolerance) closed via `SENTRIX_LEGACY_VALIDATION_HEIGHT=557144`.
- Issue #292 (RPC string consensus reporting) closed.

### Migration notes
- Hard-fork at h=579047 — pre-fork blocks (Pioneer PoA) carry forward in chain.db; post-fork blocks (Voyager DPoS+BFT) require active set sync via `/staking/validators`.

---

## [1.0.0] — 2026-04-15

Pioneer release. v1.0.0 tagged and published.

Highlights since 0.1.0:
- 7 validators across 3 VPS, full mesh peering
- CI/CD pipeline deploying to all 3 VPS with ordered stop/start and health checks
- P0 security hardening: libp2p peer limits, per-IP rate limiting, legacy TCP deprecated (PR #82)
- Full documentation suite: 20 files (PR #83)
- Pentest 6/6 passed on live production node
- P2P upgrades: bincode wire protocol, Kademlia DHT discovery, gossipsub propagation
- Disk pruning for trie roots
- 525+ tests (was 284), protocol `/sentrix/2.0.0`
- Version strings dynamic via `env!("CARGO_PKG_VERSION")`
- Network phase names: Pioneer (current), Voyager, Frontier, Odyssey
- UI branding: "Sentrix" — mainnet/testnet via chain_id (7119/7120)

---

## [0.1.0] — 2026-04-15

Pioneer phase complete.

### Added

Core: PoA round-robin consensus, account model, two-pass atomic block validation, ECDSA secp256k1 signing with chain_id, SHA-256 merkle tree, halving (42M blocks), fee split (50% burn / 50% validator), genesis premine (63M SRX), checked arithmetic everywhere.

Trie: 256-level Binary SMT (BLAKE3 leaf + SHA-256 internal), sled-backed (4 trees), LRU cache, merkle proofs, state root in block hash (post height 100K), committed root protection, GC.

Tokens: SRC-20 standard — deploy, transfer, burn, mint, approve/transferFrom. Deterministic contract addresses. Max supply enforcement. SNTX deployed (10B).

Network: libp2p (TCP + Noise XX + Yamux). Persistent Ed25519 identity. Auto-reconnect. Per-IP rate limiting (5/60s, 5-min ban). Max 50 peers. Incremental sync with sled persistence. Block processing in spawned tasks.

API: 25+ REST endpoints. 20 JSON-RPC methods (Ethereum-compatible). 12-page block explorer. Rate limiting (60/min/IP). Constant-time API key comparison. CORS restrictive. 500 concurrency, 1 MiB body, 100 batch.

Wallet: secp256k1 keygen, Keccak-256 addresses. AES-256-GCM keystore. Argon2id v2 KDF (backward-compat PBKDF2 v1). Zeroize on drop.

Storage: sled embedded DB. Per-block persistence + hash index. 1000-block sliding window.

Infra: 17 CLI commands. CI/CD (cargo deny → clippy → build → test → 3-VPS deploy). 4-phase deploy with health check. Branch protection.

Security: 11 audit rounds (94 findings, 78 fixed). Zero `unsafe`. No-panic CI enforcement. 6/6 pentest pass.

### Major PRs

| PR | What |
|----|------|
| #36–#40 | Security V4 (23 findings fixed) |
| #41 | Security V5 (11 findings) |
| #43 | Split blockchain.rs → 6 modules |
| #44 | Security V6 (13 findings) |
| #45 | libp2p integration |
| #46 | Integration tests (45 tests) |
| #48–#55 | SentrixTrie |
| #57–#60 | Security V7 |
| #65 | libp2p default (legacy TCP removed) |
| #69 | Idle timeout fix |
| #72–#73 | Security V8+V9 |
| #74 | Public repo cleanup |
| #79 | H1/H2 fork fix |
| #80 | CI/CD deploy order fix |
| #81 | Core node + 3-VPS pipeline |
| #82 | P0 security hardening |
