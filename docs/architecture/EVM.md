# EVM (Voyager Phase 2b)

Sentrix runs the Ethereum Virtual Machine via [revm](https://github.com/bluealloy/revm) 37. Solidity contracts deployed via Remix, Hardhat, or Foundry work natively. MetaMask connects via standard JSON-RPC.

## Status

- **Mainnet:** EVM disabled (`VOYAGER_EVM_HEIGHT=u64::MAX`)
- **Testnet:** EVM active since block 752

## Activation

EVM activates at the block height set by `VOYAGER_EVM_HEIGHT`. At that height:

1. `Blockchain::activate_evm()` runs once
2. All existing accounts get `code_hash = EMPTY_CODE_HASH` and `storage_root = EMPTY_STORAGE_ROOT`
3. `eth_call`, `eth_sendRawTransaction`, etc. start accepting EVM transactions
4. Block executor routes any tx with `data` field starting with `EVM:` through revm

## Account Model

```rust
pub struct Account {
    pub address: String,
    pub balance: u64,                  // sentri (1e8 = 1 SRX)
    pub nonce: u64,
    pub code_hash: [u8; 32],           // EMPTY_CODE_HASH for EOA
    pub storage_root: [u8; 32],        // EMPTY_STORAGE_ROOT for EOA
}
```

EVM tx values are denominated in **wei** at the API boundary, then converted to **sentri** internally:

```
1 SRX = 10^18 wei (Ethereum convention) = 10^8 sentri (Sentrix internal)
1 sentri = 10^10 wei
```

## Gas Model — EIP-1559

| Constant | Value |
|----------|-------|
| `INITIAL_BASE_FEE` | 10,000 sentri (0.0001 SRX) |
| `GAS_TARGET` | 15,000,000 |
| `BLOCK_GAS_LIMIT` | 30,000,000 |
| `BASE_FEE_CHANGE_DENOMINATOR` | 8 (max ±12.5% per block) |
| `MIN_BASE_FEE` | 1 sentri |

Base fee adjusts every block based on parent block utilization. Base fee is burned; priority fee goes to validator.

## JSON-RPC Endpoints

| Method | Status | Notes |
|--------|--------|-------|
| `eth_chainId` | ✓ | Returns `0x1bcf` (mainnet) or `0x1bd0` (testnet) |
| `net_version` | ✓ | Same as chainId, decimal |
| `eth_blockNumber` | ✓ | Current chain height |
| `eth_getBalance` | ✓ | Returns wei (sentri × 1e10) |
| `eth_getTransactionCount` | ✓ | Account nonce |
| `eth_getCode` | ✓ | Contract bytecode (RUNTIME, not init) |
| `eth_getStorageAt` | ✓ | Contract storage slot |
| `eth_estimateGas` | ✓ | 21K (transfer) or 100K (contract) |
| `eth_gasPrice` | ✓ | Returns 1 gwei equivalent |
| `eth_call` | ✓ | Read-only EVM execution; balance/nonce/basefee checks disabled |
| `eth_sendRawTransaction` | ✓ | Decodes legacy + EIP-1559/2930/4844/7702 |
| `eth_getBlockByNumber` | ✓ | Includes Sentrix-specific fields |
| `eth_getBlockByHash` | ✓ | |
| `eth_getTransactionByHash` | ✓ | |
| `eth_getTransactionReceipt` | ✓ | |
| `eth_syncing` | ✓ | Returns `false` (always synced) |
| `eth_accounts` | ✓ | Returns `[]` (server doesn't hold keys) |
| `web3_clientVersion` | ✓ | `Sentrix/1.2.0/Rust` |
| `net_listening` | ✓ | `true` |

## Transaction Flow

1. Client signs Ethereum tx with `eth_account` / `ethers.js` / MetaMask
2. Submit via `eth_sendRawTransaction` (RLP-encoded hex)
3. Server decodes via `alloy_consensus::TxEnvelope::decode_2718`
4. Recover sender via `SignerRecoverable::recover_signer` (k256)
5. Wrap as Sentrix `Transaction` with `data="EVM:gas:hex_calldata"` marker
6. Add to mempool (skips native sig check, allows zero-address for CREATE)
7. Block producer includes in block
8. Block executor calls `execute_evm_tx_in_block` → revm transact
9. CREATE: stores RUNTIME bytecode in `AccountDB::contract_code`, marks account contract
10. CALL: state changes applied via revm to in-memory DB

## Contract Storage

Contract bytecode + storage live in `AccountDB`:

```rust
pub contract_code: HashMap<String, Vec<u8>>,        // code_hash_hex → bytecode
pub contract_storage: HashMap<String, Vec<u8>>,     // "address:slot_hex" → value
```

After EVM CREATE succeeds, `set_contract(addr, code_hash)` marks the account as a contract.

## Precompile Addresses

Standard Ethereum precompiles (0x01-0x09) provided by revm `EthPrecompiles`:

| Address | Precompile |
|---------|-----------|
| 0x01 | ecRecover |
| 0x02 | SHA256 |
| 0x03 | RIPEMD160 |
| 0x04 | identity (data copy) |
| 0x05 | modexp |
| 0x06 | bn256Add |
| 0x07 | bn256Mul |
| 0x08 | bn256Pairing |
| 0x09 | blake2f |

Sentrix-specific precompile addresses defined for future implementation:

| Address | Purpose |
|---------|---------|
| 0x100 | Staking interaction (delegate/undelegate from contracts) |
| 0x101 | Slashing evidence submission |

## MetaMask Setup

See [MetaMask Setup Guide](../operations/METAMASK.md) for screenshots and details.

Quick add (testnet):

```
Network Name:     Sentrix Testnet
RPC URL:          https://testnet-rpc.sentriscloud.com/rpc
Chain ID:         7120
Currency Symbol:  SRX
Block Explorer:   https://sentrixscan.sentriscloud.com
```

## Known Limitations

- `eth_sendRawTransaction` accepts the tx, but receipt fields like `cumulativeGasUsed` and `logsBloom` are placeholders — full receipt indexing pending
- No EIP-4844 blob storage (decoded but blobs ignored)
- No archive node mode (only sliding window of recent state)
- Contract storage is `HashMap`, not yet integrated with SentrixTrie state root
