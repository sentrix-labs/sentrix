# Emergency Rollback Procedure

Two rollback layers exist for post-Voyager mainnet, in increasing cost /
decreasing speed:

1. **Binary rollback** (re-deploy a prior archived binary) — for a bad
   binary that hasn't corrupted state.
2. **chain.db restore** (frozen-rsync from a healthy validator) — for
   state divergence; canonical recovery, but slowest.

Always escalate from the cheapest layer first.

> **Historical note:** earlier rollback layers included a
> `SENTRIX_FORCE_PIONEER_MODE=1` env-var override that forced the
> binary back to Pioneer PoA, used during the 2026-04-25 Voyager
> activation #1 livelock. Now obsolete — the L1 multiaddr advertisements
> + L2 cold-start gate (v2.1.26 / v2.1.27) make the activation-time
> livelock failure mode unreachable, and the override has been removed
> from all production env files. If a future BFT-class livelock occurs
> on a steady-state chain, prefer chain.db rsync from a healthy peer
> over forcing back to Pioneer.

---

## 1. Binary Rollback (no state corruption)

Each validator's deploy archives the previous binary under
`<bin_dir>/releases/` (last 3 retained). To roll back, re-run your
deploy with a prior archive instead of building a fresh binary.

For the maintainer fleet, use the private orchestrator with the
`SENTRIX_ROLLBACK` env var pointing at the archived binary path:

```bash
SENTRIX_ROLLBACK=/opt/sentrix/releases/sentrix-vX.Y.Z-<timestamp> \
  <orchestrator> mainnet
```

The orchestrator skips the build step, ships the named binary, does
the same rolling stop/start order with health check, ~2 min end-to-end.

For third-party validators (single host), use
`scripts/deploy-validator.sh` with `--binary` pointing at the archive:

```bash
./scripts/deploy-validator.sh \
  --host operator@validator.example.com --service sentrix-node \
  --bin-dir /opt/sentrix --rpc-url http://127.0.0.1:8545 \
  --binary /opt/sentrix/releases/sentrix-vX.Y.Z-<timestamp>
```

Manual single-host fallback (any operator):

```bash
# 1. Stop the unit
sudo systemctl stop <validator-service>

# 2. List archived versions
ls -lt <bin_dir>/releases/

# 3. Restore (use install/mv-rename, NOT cp — running binaries trip ETXTBSY)
sudo install -m 755 <bin_dir>/releases/sentrix-vX.Y.Z-<timestamp> <bin_dir>/sentrix

# 4. Restart
sudo systemctl start <validator-service>
```

Current production binary at the time of writing: **v2.1.38** (mainnet
& testnet, post-libp2p-sync-cascade-bail-fix). Prior production releases
archived under each validator's `<bin_dir>/releases/`: v2.1.37, v2.1.36,
v2.1.35, v2.1.34, v2.1.33, v2.1.32, v2.1.31, v2.1.30, v2.1.29, v2.1.28,
v2.1.27, v2.1.26.

The 2026-04-25 / 2026-04-26 incident hotfix series:
- v2.1.31: BFT signing v2 foundation + Frontier F-2 shadow + libp2p connection-leak fix
- v2.1.32: `/p2p/<peer_id>` in advert multiaddrs (closes #319 partial-fix gap)
- v2.1.33: voyager_mode_for runtime-aware check + connection_limits Behaviour
- v2.1.34: connection_limits cap loosened 1→2 (production hotfix)
- v2.1.35: Voyager-mode-for migration sweep + claim-rewards tool
- v2.1.36: tx validate exempts staking ops from amount>0 check (ClaimRewards submission fix)
- v2.1.37: libp2p sync cascade-bail filter (P0: 2026-04-26 mainnet stall at h=604547 root cause + fix). Recovered via Treasury-canonical chain.db rsync. See PR #334 + RCA at `incidents/2026-04-26-libp2p-sync-cascade-bail-stall.md` (founder-private).
- v2.1.38: legacy TCP-path deletion (sync.rs + node.rs trimmed) + cumulative skip-counter observability for race re-emergence detection

---

## 2. State Recovery (chain.db restore)

When state has diverged (different block hash at the same height,
state_root mismatch, etc.), the canonical recovery is a **frozen
rsync** of `chain.db` from a healthy peer with **all** validators
halted. See [STATE_EXPORT.md](STATE_EXPORT.md) for why
`sentrix state export/import` is **not** the right path for a
post-genesis chain.

The full procedure lives in `internal operator runbook`
(internal). Outline:

1. Pick the canonical validator (matches the most peers; longest valid
   chain at consensus root; for BFT-finalized chains, prefer the one whose
   justification signer-set matches the majority of healthy peers).
2. Stop **all** validators on the diverged hosts.
3. Backup the diverged `chain.db` to a sibling directory:
   `sudo cp -a <data_dir>/chain.db <data_dir>/chain.db.divergent-<height>-<ts>`
4. Tar-pipe the canonical `chain.db` to each diverged host while
   the source is frozen (canonical node is also stopped during the
   copy):
   ```bash
   ssh <canonical> "sudo tar -C <canonical_data_dir> -cf - chain.db" | \
     ssh <dest> "sudo tar -C <dest_data_dir> -xf - --no-same-owner --no-same-permissions"
   ```
   Why tar-pipe over rsync: chain.db is a directory of MDBX files; tar
   handles ownership normalization with `--no-same-owner` cleanly when
   source/destination users differ.
5. `chown -R sentriscloud:sentriscloud <data_dir>/chain.db` (or whichever
   user owns the running daemon).
6. **MD5 parity check** — verify all destinations have identical
   `mdbx.dat`:
   `sudo md5sum <data_dir>/chain.db/mdbx.dat`
7. Start validators in the standard producer order (most-recently-active
   first to anchor BFT round numbering, then peers).

State_root is recomputed from the canonical chain.db on the next block
and the divergence is gone.

> **Worked example (2026-04-26 mainnet stall, h=604547).** All 4
> validators had different block hashes at h=604547. Treasury picked as
> canonical (most progressed at h=604548, self-consistent prev-link,
> justification signer-set matched majority). Tar-pipe Treasury chain.db
> → Foundation, Core, Beacon. MD5 parity confirmed
> (`mdbx.dat` md5 = `567c7165301fff7e95ded23d03df63cd`). Restart Treasury
> → Foundation → Core → Beacon. Chain advanced past h=604548 within
> seconds. Per-validator hash parity verified at h=604650. RCA in
> `incidents/2026-04-26-libp2p-sync-cascade-bail-stall.md` (founder-private).

---

## NEVER Do This

- **Never `git push --force` to roll back.** The CI/CD deploy job is
  disabled — a force-push to main does **not** redeploy. Re-run your
  deploy with `SENTRIX_ROLLBACK=<archived-binary-path>` instead.
  Force-push also rewrites public history; CI test artifacts and
  PR-comment links start pointing at vanished commits.

- **Never build on Windows and SCP to Linux validators.** Windows
  produces PE executables, Linux needs ELF. The binary will fail with
  "Exec format error". Always build inside a Linux container (e.g.
  `rust:1.95-bullseye` for glibc 2.31 compat across modern Ubuntu/Debian).

- **Never run admin CLI separately per-VPS during recovery.** Run the
  validator add/remove/toggle on a single chain.db, then rsync to the
  rest. The admin_log holds wall-clock timestamps; running the same op
  three times produces three different timestamps and three different
  state_roots.

- **Never use `sentrix state export/import` to recover a post-genesis
  chain.** v2.1.5+ refuses to start on a keystore built from import.
  Use frozen-rsync of chain.db (path 3 above).
