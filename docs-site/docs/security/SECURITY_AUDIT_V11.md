# Security Audit V11 — Code Review

**Scope:** 39 files, ~6,500 lines. Manual line-by-line review.

## TL;DR

0 critical, 2 high, 5 medium, 7 low, 8 info (positive findings). Overall **8.3/10**. No fund-loss vulnerabilities. Main concerns are fee tracking architecture and timestamp determinism.

Good stuff found: zero `unsafe`, CI-enforced no-panic (clippy deny unwrap/expect/panic), checked math everywhere, constant-time API key comparison, Argon2id keystore, committed root protection in trie, canonical BTreeMap signing payload, pubkey→address verified on every tx.

---

## HIGH

### H01: Fee Burn Tracked in Two Places

`AccountDB::transfer()` tracks `ceil(fee/2)` as burned. Then `add_block()` credits `floor(fee/2)` to validator. The math works out — net balances are correct. But the logic is split across two modules which makes it confusing to reason about.

Files: `account.rs`, `block_executor.rs`
Fix: Consolidate fee handling into one place.

### H02: Block Timestamp Not Deterministic

`SystemTime::now()` called twice — once for coinbase in `create_block()`, once for the block in `Block::new()`. They can differ. Means same logical block → different hash.

Not exploitable in PoA (one validator per slot), but matters if a validator restarts mid-slot.

Files: `block_producer.rs`, `block.rs`
Fix: Capture timestamp once, pass to both.

---

## MEDIUM

| ID | Finding | File |
|----|---------|------|
| M01 | No fork choice rule — first block at a height wins, no reorg | `block_executor.rs` |
| M02 | Mempool nonce = on-chain + pending count. If a pending tx expires, subsequent txs get wrong nonce | `mempool.rs` |
| M03 | Legacy TCP broadcasts open new connection per peer per message (~1000 conn/min with 50 peers) | `node.rs` |
| M04 | `jsonrpc_handler()` has no auth — relies on route-level dispatcher. If exposed directly, auth bypassed | `jsonrpc.rs` |
| M05 | State root check happens AFTER Pass 2 commits state. Mismatch = error returned but state already mutated | `block_executor.rs` |

---

## LOW

| ID | Finding |
|----|---------|
| L01 | Block timestamp future window 15s = 5x block interval. Could tighten to 6s |
| L02 | Admin address is just a string — no crypto binding to a real keypair |
| L03 | Coinbase recipient not validated against `block.validator` |
| L04 | Token contract address collision loop is unbounded (theoretical) |
| L05 | `get_transaction` does O(N) linear scan — no txid index |
| L06 | Rate limiter fallback is `true` (allow) if extension missing |
| L07 | Peer height in HashMap never updated after initial handshake |

---

## INFO (Good Findings)

| ID | What | Verdict |
|----|------|---------|
| I01 | `signing_payload()` falls back to `"{}"` — practically impossible | Fine |
| I02 | Genesis has hardcoded timestamp — deterministic | Good ✓ |
| I03 | Clippy deny unwrap/expect/panic — CI enforced | Excellent ✓ |
| I04 | `Zeroizing<[u8; 32]>` for private key, no Clone | Good ✓ |
| I05 | BLAKE3 leaf + SHA-256 internal — domain separated | Good ✓ |
| I06 | CORS restrictive by default | Good ✓ |
| I07 | Approve requires reset to 0 first — prevents front-run | Good ✓ |
| I08 | HTML escaping on all explorer output — no XSS | Good ✓ |

---

## Scores

| Category | Score |
|----------|-------|
| Consensus | 8/10 |
| State management | 9/10 |
| Transactions | 9/10 |
| Networking | 7/10 |
| API security | 8/10 |
| Code quality | 9/10 |
| **Overall** | **8.3/10** |
