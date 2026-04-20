# Sentrix Transaction Tracing — RPC Spec

Sentrix exposes two tracing RPC methods that give clients step-by-step
insight into what a transaction actually did. They serve different
audiences because Sentrix runs two distinct execution paths:

- **`debug_traceTransaction`** — EVM bytecode trace, opcode level.
  Ethereum-standard. Applies to transactions executed by the embedded
  revm EVM (active on testnet; on mainnet after the Voyager fork).
- **`sentrix_traceTransaction`** — native-operation trace, state-diff
  level. Sentrix-native. Applies to native transactions (SRX
  transfers, SRC-20 deploy/transfer/mint/burn/approve, staking ops).

Both methods are read-only, safe to call on any full node, and live
under the standard JSON-RPC endpoint at `POST /rpc`.

> Scope of this doc: request/response shape, use-cases, guarantees.
> Implementation and test plans live in the internal engineering
> notes; see the backlog items and CHANGELOG entries for each shipped
> version.

---

## 1. `debug_traceTransaction`

Matches the Ethereum `debug` namespace method of the same name so
Hardhat / Foundry / Remix / Tenderly-compatible tooling works
unmodified against a Sentrix EVM-enabled endpoint.

### 1.1 Request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "debug_traceTransaction",
  "params": [
    "0x<txid>",
    {
      "tracer": "callTracer",                 // optional
      "tracerConfig": { "onlyTopCall": false },
      "timeout": "10s",                        // optional, max 30s
      "disableStack": false,                   // struct-log only
      "disableMemory": false,                  // struct-log only
      "disableStorage": false                  // struct-log only
    }
  ]
}
```

### 1.2 Tracer modes

| `tracer` value | Output | Typical size | When to use |
|---|---|---|---|
| _omitted_ (default) | `StructLog` — full opcode-by-opcode trace | 10 KB – 100 MB per tx | Deep debugging, gas analysis |
| `callTracer` | Call-graph tree (CALL / DELEGATECALL / CREATE / STATICCALL) | 1 – 100 KB | Understand flow, not opcodes |
| `prestateTracer` | Accounts / storage touched by the tx, pre-state values | 1 – 500 KB | Replay testing, fork simulation |

### 1.3 Response — struct logger (default)

```json
{
  "gas": 21000,
  "failed": false,
  "returnValue": "",
  "structLogs": [
    {
      "pc": 0,
      "op": "PUSH1",
      "gas": 21000,
      "gasCost": 3,
      "depth": 1,
      "stack": ["0x80"],
      "memory": [],
      "storage": {}
    },
    ...
  ]
}
```

Per-step fields match the Ethereum spec exactly. `disableStack` /
`disableMemory` / `disableStorage` options let clients trim output
when they only need certain dimensions (common for gas-profiling
tools that don't care about memory).

### 1.4 Response — `callTracer`

```json
{
  "type": "CALL",
  "from": "0xaa...",
  "to":   "0xbb...",
  "value": "0x0",
  "gas":   "0x520f",
  "gasUsed": "0x520f",
  "input": "0xa9059cbb...",
  "output": "0x",
  "calls": [
    {
      "type": "DELEGATECALL",
      "from": "0xbb...",
      "to":   "0xcc...",
      "gas": "0x4e20",
      "gasUsed": "0x4e20",
      "calls": []
    }
  ]
}
```

### 1.5 Response — `prestateTracer`

```json
{
  "0xaa...": {
    "balance": "0xde0b6b3a7640000",
    "nonce": 5
  },
  "0xbb...": {
    "balance": "0x0",
    "code": "0x6080604052...",
    "storage": {
      "0x0000...0001": "0x0000...0100"
    }
  }
}
```

### 1.6 Errors

| Code | Message | Meaning |
|---|---|---|
| `-32602` | `invalid params` | Bad txid / malformed tracer config |
| `-32000` | `transaction not found` | txid not in chain or mempool |
| `-32000` | `tx is not an EVM transaction` | txid resolves to a native op; use `sentrix_traceTransaction` |
| `-32000` | `trace timeout` | Output exceeded `timeout` (default 10s, max 30s) |
| `-32000` | `output too large` | StructLog output over the configured cap (default 50 MB) |

### 1.7 Guarantees

- **Deterministic.** Same txid on any synced node returns byte-identical
  output. Re-execution uses the same block context, gas limit, and
  pre-state.
- **Read-only.** Never mutates chain state.
- **Chain-range.** Available for every EVM tx whose block is still in
  the in-memory window (`CHAIN_WINDOW_SIZE`) or resolvable through
  the `txid_index` in MDBX.
- **Post-fork only.** Trace requests for blocks before the EVM fork
  height (`VOYAGER_EVM_HEIGHT`) return `tx is not an EVM transaction`
  even if the txid is in a valid pre-fork block.

### 1.8 Tooling compatibility

Confirmed to work after implementation:

- **Hardhat** `network.provider.send("debug_traceTransaction", ...)`
- **Foundry** `forge debug <txid> --rpc-url <sentrix>`
- **Remix IDE** debugger tab (attach to RPC, paste txid)
- **Tenderly**-pattern indexers (commercial services that batch-trace)
- **ethers.js v6** via provider's `send("debug_traceTransaction", ...)`

---

## 2. `sentrix_traceTransaction`

Sentrix-native equivalent for non-EVM transactions. Returns an
operation-level trace rather than opcode-level — native operations
don't have a VM to step through; they run directly in Rust, so the
valuable view is *what state changed*, not *which opcodes ran*.

### 2.1 Request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sentrix_traceTransaction",
  "params": ["0x<txid>"]
}
```

No tracer config — there's only one level of detail that makes sense
for native ops.

### 2.2 Response — native SRX transfer

```json
{
  "op": "NativeTransfer",
  "from": "0xaa...",
  "to":   "0xbb...",
  "amount_sentri": 100000000,
  "amount_srx": 1.0,
  "fee_sentri": 1000000,
  "pre_state": {
    "0xaa...": { "balance_sentri": 5000000000, "nonce": 42 },
    "0xbb...": { "balance_sentri": 0,          "nonce": 0 }
  },
  "post_state": {
    "0xaa...": { "balance_sentri": 4899000000, "nonce": 43 },
    "0xbb...": { "balance_sentri": 100000000,  "nonce": 0 }
  },
  "fee_split": {
    "burned_sentri": 500000,
    "validator_sentri": 500000,
    "validator": "0xvalidator..."
  }
}
```

### 2.3 Response — SRC-20 transfer

```json
{
  "op": "SRC20Transfer",
  "contract": "SRC20_...",
  "from": "0xaa...",
  "to":   "0xbb...",
  "amount": "1000000000000000000",
  "pre_balances": {
    "0xaa...": "5000000000000000000",
    "0xbb...": "0"
  },
  "post_balances": {
    "0xaa...": "4000000000000000000",
    "0xbb...": "1000000000000000000"
  },
  "token": {
    "symbol": "SNTX",
    "decimals": 18,
    "total_supply_before": "10000000000000000000000",
    "total_supply_after":  "10000000000000000000000"
  },
  "events": [
    { "type": "Transfer", "from": "0xaa...", "to": "0xbb...", "amount": "1000000000000000000" }
  ]
}
```

### 2.4 Response — SRC-20 mint / burn

Same shape as transfer, plus `total_supply_before` / `total_supply_after`
that differ (mint grows supply; burn shrinks it).

### 2.5 Response — SRC-20 deploy

```json
{
  "op": "SRC20Deploy",
  "contract": "SRC20_abc123...",
  "deployer": "0xaa...",
  "token": {
    "name": "Sentrix Utility Token",
    "symbol": "SNTX",
    "decimals": 18,
    "initial_supply": "10000000000000000000000",
    "max_supply": "10000000000000000000000"
  },
  "genesis_holder": "0xaa..."
}
```

### 2.6 Response — staking ops (Voyager mode only)

`Delegate`, `Undelegate`, `Redelegate`, `ClaimRewards`,
`RegisterValidator`, `UpdateCommission` — each returns its relevant
pre/post state (stake amounts, unbonding queue entries, validator
commission).

### 2.7 Errors

| Code | Message |
|---|---|
| `-32602` | `invalid params` |
| `-32000` | `transaction not found` |
| `-32000` | `tx is an EVM transaction — use debug_traceTransaction` |

### 2.8 Use cases

1. **Explorer UI** — Sentrix Scan per-tx page showing what changed
   (balances before/after, events emitted, fee split), not just raw
   payload.
2. **Wallet confirmation UX** — show the user "you are about to send
   1000 SNTX, reducing your balance from X to Y" before they sign,
   cuts down on phishing / approval mistakes.
3. **Integration tests** — assert on state diffs in Rust / JS tests
   without a separate balance-fetch step.
4. **Token creator dashboards** — issuers monitor `mint` / `burn` ops
   and supply trajectory.
5. **Forensic audit trail** — AML / compliance investigators follow
   operation sequences with structured pre/post state.
6. **Subgraph / indexer efficiency** — the trace output is already
   structured per op type, so indexers don't re-parse transaction
   data.

### 2.9 Guarantees

- **Deterministic**, **read-only**, **chain-range** — same as
  `debug_traceTransaction`.
- **Cheap** — output size is bounded per op type (max ~5 KB even for
  complex staking ops), unlike EVM trace output.
- **No pre-fork gating** — native ops have existed since genesis, so
  any txid in the chain is traceable.

---

## 3. Method selection guide

| What the tx is | Use |
|---|---|
| Native SRX transfer | `sentrix_traceTransaction` |
| SRC-20 deploy / transfer / mint / burn / approve | `sentrix_traceTransaction` |
| Staking / delegation op | `sentrix_traceTransaction` |
| Smart contract deploy (EVM CREATE) | `debug_traceTransaction` |
| Smart contract call (ERC-20 transfer on Uniswap, DEX swap, etc.) | `debug_traceTransaction` |
| Raw Ethereum tx (legacy / EIP-1559 / EIP-2930) | `debug_traceTransaction` |

If the tx type is unknown, clients can call either method — the one
that doesn't apply returns `-32000` with a pointer to the right
method, letting the client auto-dispatch.

---

## 4. Rollout

Both methods are slated for implementation after the Voyager-phase
reward distribution fix lands (it's a consensus-critical prerequisite).
They are non-consensus RPC additions, so they can ship in a minor
version and be deployed to a subset of nodes for early testing before
fleet-wide rollout.

- **`sentrix_traceTransaction`** — smaller scope, targets the native
  explorer / wallet path, ~1–2 days implementation. Prioritised first
  because mainnet is currently PoA + native-only, so it's immediately
  useful; EVM trace won't be until mainnet Voyager fork.
- **`debug_traceTransaction`** — ~4–5 days implementation (struct
  logger + callTracer + prestateTracer modes), targets the EVM
  developer ecosystem. Prioritised alongside EVM activation on
  mainnet.
