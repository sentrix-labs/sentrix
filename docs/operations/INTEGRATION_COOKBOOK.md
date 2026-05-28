# Integration Cookbook

Code recipes for integrating Sentrix Chain into a dApp, indexer, or backend. Sentrix is EVM-compatible (chain ID `7119` mainnet / `7120` testnet) and works with the standard Ethereum tooling — `viem`, `wagmi`, `ethers`, `hardhat`, `foundry`. This page collects the snippets so you can copy-paste against the canonical RPC URLs without hand-rolling the chain config.

For UI-only walkthroughs (MetaMask + Remix), see [DEVELOPER_QUICKSTART.md](DEVELOPER_QUICKSTART.md) and [SMART_CONTRACT_GUIDE.md](SMART_CONTRACT_GUIDE.md). For native-side integration (REST, gRPC, WebSocket), see [API_REFERENCE.md](API_REFERENCE.md) and [GRPC.md](GRPC.md).

## Network reference

| Network | Chain ID | JSON-RPC | WSS | Explorer |
|---|---|---|---|---|
| Mainnet | `7119` | `https://rpc.sentrixchain.com/rpc` | `wss://rpc.sentrixchain.com/ws` | [scan.sentrixchain.com](https://scan.sentrixchain.com) |
| Testnet | `7120` | `https://testnet-rpc.sentrixchain.com/rpc` | `wss://testnet-rpc.sentrixchain.com/ws` | [scan.sentrixchain.com](https://scan.sentrixchain.com) |

JSON-RPC also responds at the bare host (POST to `https://rpc.sentrixchain.com`) — both paths reach the same node.

---

## viem

`viem 2.50.4+` ships Sentrix chains in `viem/chains` — `import { sentrix, sentrixTestnet }` resolves with the correct `id`, `rpcUrls`, `blockExplorers`, and `nativeCurrency` already wired.

```ts
import { createPublicClient, http } from 'viem'
import { sentrix, sentrixTestnet } from 'viem/chains'

const client = createPublicClient({
  chain: sentrix,                                  // or sentrixTestnet
  transport: http(),                               // uses the chain's default RPC
})

const blockNumber = await client.getBlockNumber()
const balance = await client.getBalance({ address: '0x...' })
```

For a custom RPC override (e.g. private endpoint):

```ts
const client = createPublicClient({
  chain: sentrix,
  transport: http('https://your-private-rpc.example.com'),
})
```

## wagmi

`wagmi` consumes viem's chain definitions, so the same `sentrix` / `sentrixTestnet` exports work directly.

```ts
import { http, createConfig } from 'wagmi'
import { sentrix, sentrixTestnet } from 'viem/chains'
import { injected, walletConnect } from 'wagmi/connectors'

export const config = createConfig({
  chains: [sentrix, sentrixTestnet],
  connectors: [
    injected(),                                    // MetaMask + any EIP-1193 wallet
    walletConnect({ projectId: 'YOUR_PROJECT_ID' }),
  ],
  transports: {
    [sentrix.id]: http(),
    [sentrixTestnet.id]: http(),
  },
})
```

In a React component:

```tsx
import { useAccount, useBalance, useReadContract } from 'wagmi'

function Balance() {
  const { address } = useAccount()
  const { data: balance } = useBalance({ address })
  return <div>{balance?.formatted} {balance?.symbol}</div>
}
```

Frameworks built on wagmi (RainbowKit, Web3Modal, ConnectKit) work transparently — they read the chain registry from wagmi's config.

## ethers v6

ethers does not ship Sentrix chain definitions yet. Define them manually:

```ts
import { JsonRpcProvider, Network } from 'ethers'

const sentrixTestnet = new Network('sentrix-testnet', 7120n)
const provider = new JsonRpcProvider(
  'https://testnet-rpc.sentrixchain.com/rpc',
  sentrixTestnet,
  { staticNetwork: sentrixTestnet },               // avoid auto-detect roundtrip
)

const blockNumber = await provider.getBlockNumber()
const balance = await provider.getBalance('0x...')
```

For mainnet replace the URL with `https://rpc.sentrixchain.com/rpc` and the chain ID with `7119n`.

## Canonical contracts (npm)

`@sentrix-labs/canonical-contracts` ships as-const ABIs + per-chain addresses for WSRX, Multicall3, SentrixSafe, and TokenFactory. Install:

```sh
npm install @sentrix-labs/canonical-contracts
```

Use with viem:

```ts
import { WSRX_ABI, WSRX_ADDRESS } from '@sentrix-labs/canonical-contracts'
import { createPublicClient, http } from 'viem'
import { sentrix } from 'viem/chains'

const client = createPublicClient({ chain: sentrix, transport: http() })

const totalSupply = await client.readContract({
  address: WSRX_ADDRESS[sentrix.id],               // 0x4693… on mainnet
  abi: WSRX_ABI,
  functionName: 'totalSupply',
})
```

ABIs are typed `as const`, so viem / wagmi infer the full call / event surface without manual type annotations.

## Hardhat

Add Sentrix to `hardhat.config.ts`:

```ts
import { HardhatUserConfig } from 'hardhat/config'
import '@nomicfoundation/hardhat-toolbox'

const config: HardhatUserConfig = {
  solidity: '0.8.24',
  networks: {
    sentrix: {
      url: 'https://rpc.sentrixchain.com/rpc',
      chainId: 7119,
      accounts: [process.env.PRIVATE_KEY!],
    },
    sentrixTestnet: {
      url: 'https://testnet-rpc.sentrixchain.com/rpc',
      chainId: 7120,
      accounts: [process.env.PRIVATE_KEY!],
    },
  },
}
export default config
```

Deploy:

```sh
npx hardhat run scripts/deploy.ts --network sentrixTestnet
```

For a fuller starter project (Hardhat + viem + sample contracts + Sourcify verification), clone [dApp Starter](https://github.com/SentrisCloud/dapp-starter).

## Foundry

Set the RPC + chain via env or `foundry.toml`:

```toml
[rpc_endpoints]
sentrix         = "https://rpc.sentrixchain.com/rpc"
sentrix_testnet = "https://testnet-rpc.sentrixchain.com/rpc"
```

Deploy with `forge create`:

```sh
forge create --rpc-url sentrix_testnet \
             --private-key $PRIVATE_KEY \
             src/MyContract.sol:MyContract
```

Or with `forge script`:

```sh
forge script script/Deploy.s.sol \
  --rpc-url sentrix_testnet \
  --private-key $PRIVATE_KEY \
  --broadcast
```

`cast` works for ad-hoc queries:

```sh
cast block-number --rpc-url https://rpc.sentrixchain.com/rpc
cast balance 0x... --rpc-url https://rpc.sentrixchain.com/rpc
cast call 0x... 'totalSupply()(uint256)' --rpc-url https://rpc.sentrixchain.com/rpc
```

## SDKs (first-party)

For projects that need the Sentrix-specific surface (native REST, BFT subscriptions, gRPC, staking ops) on top of EVM:

| SDK | Repo | When to use |
|---|---|---|
| `@sentriscloud/sdk-ts` | [SentrisCloud/sdk-ts](https://github.com/SentrisCloud/sdk-ts) | TypeScript projects — typed wrappers over EVM JSON-RPC, native REST, and WebSocket subscription helpers. |
| `sentrix-chain` (Rust) | [SentrisCloud/sdk-rs](https://github.com/SentrisCloud/sdk-rs), [crates.io](https://crates.io/crates/sentrix-chain) | Rust projects — typed clients for native REST, EVM (via alloy), gRPC (via tonic), and secp256k1 wallet/signing. |
| `sentrix-grpc-wasm` | [SentrisCloud/sentrix-grpc-wasm](https://github.com/SentrisCloud/sentrix-grpc-wasm) | Browser dApps that need gRPC-Web without a Node middleman. |

EVM-only dApps generally do not need a Sentrix SDK — `viem` or `ethers` is enough.

## WebSocket subscriptions

Sentrix exposes both standard `eth_subscribe` channels and Sentrix-specific `sentrix_subscribe` channels (`sentrix_finalized`, `sentrix_validatorSet`, `sentrix_tokenOps`, `sentrix_stakingOps`, `sentrix_jail`). See [WEBSOCKET_SUBSCRIPTIONS.md](WEBSOCKET_SUBSCRIPTIONS.md) for the full channel catalog.

Quick sample with viem:

```ts
import { createPublicClient, webSocket } from 'viem'
import { sentrix } from 'viem/chains'

const client = createPublicClient({
  chain: sentrix,
  transport: webSocket('wss://rpc.sentrixchain.com/ws'),
})

const unwatch = client.watchBlocks({
  onBlock: (block) => console.log('new block', block.number),
})
```

## Common gotchas

- **`eth_getTransactionByHash` returns a chain-native shape** (with `EVM:` data prefix and Sentrix-specific fields) instead of the strict Ethereum JSON-RPC envelope. Standard ethers/viem decoders crash on it; use `cast` for ad-hoc inspection or the typed SDK helpers. Tracked in [sentrix#680](https://github.com/sentrix-labs/sentrix/issues/680).
- **`eth_call` against a past block tag** historically returned `-32004` on older nodes; fixed in `v2.2.15+`. If your indexer queries `eth_call` with `blockNumber` (rather than `latest`), make sure the node is `>= 2.2.15`.
- **EVM transactions with non-zero `msg.value`** require the chain to be past `EVM_VALUE_TRANSFER_HEIGHT` (mainnet `1,748,900`, activated 2026-05-13). Pre-fork, payable internal-call chains reverted at 27k gas. Detail in [sentrix#580](https://github.com/sentrix-labs/sentrix/issues/580).
- **Block time is ~1–2 s on mainnet**; finality is BFT — a block is final the moment it carries a 2/3+1 stake-weighted precommit set. Polling `eth_blockNumber` once per second is fine; tighter polling wastes round-trips.

## Where to go next

- **Deploy a contract end-to-end (UI flow)** — [SMART_CONTRACT_GUIDE.md](SMART_CONTRACT_GUIDE.md)
- **Verify a deployed contract** — [CONTRACT_VERIFICATION.md](CONTRACT_VERIFICATION.md)
- **Stream finalised blocks over gRPC** — [GRPC.md](GRPC.md)
- **Query native REST endpoints** — [API_ENDPOINTS.md](API_ENDPOINTS.md), [API_REFERENCE.md](API_REFERENCE.md)
- **Listen to BFT / staking events** — [WEBSOCKET_SUBSCRIPTIONS.md](WEBSOCKET_SUBSCRIPTIONS.md)
- **Run your own RPC node** — [VALIDATOR_ONBOARDING.md](VALIDATOR_ONBOARDING.md) (the fullnode shape lives in the same binary)

## See also

- [Sentrix Chain repo](https://github.com/sentrix-labs/sentrix) — core node implementation.
- [Canonical contracts repo](https://github.com/sentrix-labs/canonical-contracts) — WSRX, Multicall3, SentrixSafe, TokenFactory.
- [dApp Starter](https://github.com/SentrisCloud/dapp-starter) — Hardhat + viem template, deploys WSRX wrap + ERC-20 example.
- [Awesome Sentrix](https://github.com/sentrix-labs/awesome-sentrix) — curated index of every Sentrix-related resource.
