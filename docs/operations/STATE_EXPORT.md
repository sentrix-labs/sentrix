# State Export & Import

Backup, restore, and migrate Sentrix chain state.

> **Critical limitation (v2.1.5+):** `sentrix state import` is
> **effectively deprecated for non-genesis use**. Since v2.1.5 the
> binary refuses to start on a keystore built from `state import` on a
> post-genesis chain — the import path skips trie/admin_log artefacts
> the boot validator now requires. The canonical state-recovery path
> is **frozen-rsync of `chain.db`** from a healthy validator with all
> nodes halted; see
> [EMERGENCY_ROLLBACK.md § 3](EMERGENCY_ROLLBACK.md#3-state-recovery-chaindb-restore)
> and the internal
> `founder-private/runbooks/state-divergence-recovery.md`.
>
> Export remains useful for read-only inspection, archival snapshots,
> and bootstrapping a **fresh** chain (genesis import). The commands
> below are still the right tool for those use cases — just not for
> recovering a live mainnet validator.

## Export

```bash
# Export current state to JSON (run while node is STOPPED)
sentrix state export --output backup.json

# Output includes: accounts, balances, contracts, storage, validators
```

The snapshot file is a human-readable JSON containing:

- **Metadata:** chain_id, height, block_hash, timestamp
- **Accounts:** address, balance, nonce, code_hash, storage_root
- **Contract code:** bytecode indexed by code_hash
- **Contract storage:** key-value pairs per contract
- **Validators:** active set with names, addresses, public keys, stats
- **Counters:** total_minted, total_burned

## Import

```bash
# Import state from snapshot (DESTRUCTIVE — replaces all current state)
sentrix state import backup.json --force

# After import, restart the node to rebuild the trie
sudo systemctl restart sentrix-validator
```

## Verify

```bash
# Check integrity without importing
sentrix state verify backup.json
# → "Snapshot v1 OK: chain_id=7119 height=200000 accounts=150 validators=7..."
```

## Use Cases

| Use Case | Command |
|----------|---------|
| Backup before upgrade | `sentrix state export --output pre_upgrade.json` |
| Bootstrap new node | Export from healthy node → import on new node |
| Fork for testing | Export mainnet state → import on testnet chain_id |
| Archive historical state | Export at each milestone height |
| Incident recovery | Import from last known-good snapshot |

## Mempool Management

```bash
# View mempool stats (run while node is stopped)
sentrix mempool stats

# Clear stuck transactions (run while node is stopped)
sentrix mempool clear
```

The `mempool clear` command is useful after a stuck-mempool incident (e.g. batch of invalid-nonce transactions blocking block production).

## Notes

- Export must run while the node is **stopped** (MDBX uses an exclusive lock).
- Import is **destructive** — all current accounts/validators/contracts are replaced.
- Block history is NOT exported — only the latest state. The node will resync history from peers after restart.
- Snapshot files can be large for chains with many contracts. Use gzip for archival: `gzip backup.json`.
