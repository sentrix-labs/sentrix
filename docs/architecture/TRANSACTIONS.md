# Transactions

## Types

| Type | From | To | Who creates it |
|------|------|----|----------------|
| Coinbase | `COINBASE` | Validator | Automatic (block reward) |
| Transfer | User | Recipient | User (signed) |
| Token op | User | Contract | User (via SRX-20) |

## Structure

```rust
Transaction {
    txid,           // SHA-256 of signing payload
    from_address,   // 0x + 40 hex
    to_address,
    amount,         // sentri (u64)
    fee,            // sentri (min 10,000 = 0.0001 SRX)
    nonce,          // sequential per sender
    timestamp,
    signature,      // ECDSA hex
    public_key,     // secp256k1 hex
    chain_id,       // 7119
    data,           // optional — token ops (JSON)
}
```

## Signing

ECDSA on secp256k1 (same as Bitcoin/Ethereum).

Payload built from `BTreeMap` (sorted keys → deterministic JSON). The `txid` is SHA-256 of that payload.

On verify: recover pubkey from signature, derive address via Keccak-256, check it matches `from_address`. Can't forge txs from someone else's address.

Chain ID (7119) in the signing payload = replay protection across networks.

## Mempool

Where txs wait before getting into a block.

### Rules to get in

- Valid signature, pubkey derives to from_address
- Nonce = `on_chain_nonce + pending_count`
- Balance ≥ amount + fee + pending spends
- Fee ≥ 10,000 sentri (0.0001 SRX)
- Valid address format, not zero address
- Amount > 0 (except token ops)
- Timestamp not >5min future, not >1hr old
- No duplicate txid
- Chain ID = 7119

### Limits

| | |
|-|-|
| Total | 10,000 txs |
| Per sender | 100 txs |
| Max age | 1 hour (auto-pruned) |
| Per block | 100 txs |

Higher-fee txs get picked first.

## Fees

Min fee: 0.0001 SRX (10,000 sentri).

```
Fee → ceil(fee/2) burned + floor(fee/2) to validator
```

50% burn creates deflationary pressure. Eventually burns > rewards = net deflation.

## Nonce

Sequential counter per address, starting at 0. Must be exact — gaps rejected. Prevents replay within the same chain.

## Coinbase

One per block, always first tx:
- From: `"COINBASE"`
- Amount: block reward (1 SRX in Era 0)
- Fee: 0, nonce: 0, no signature

## Token Operations

Encoded in the `data` field as JSON:

```json
{"op": "deploy",   "name": "...", "symbol": "...", "supply": N, "decimals": N}
{"op": "transfer", "contract": "SRX20_...", "to": "0x...", "amount": N}
{"op": "burn",     "contract": "SRX20_...", "amount": N}
{"op": "approve",  "contract": "SRX20_...", "spender": "0x...", "amount": N}
```

Still need SRX for gas. See [Token Standards](../tokenomics/TOKEN_STANDARDS.md).

## Overflow Protection

All monetary math uses `checked_add`/`checked_sub`. Returns error on overflow/underflow. No wrapping, no unchecked arithmetic on balances.
