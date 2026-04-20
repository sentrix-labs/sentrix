# Sentrix JS/TS Integration Snippets

Drop-in code for frontend integrators. Covers the three ways you'll talk to the chain:

1. **REST** via `fetch` / `axios` — for `/accounts/*`, `/tokens/*`, `/chain/info`, etc.
2. **JSON-RPC via fetch** — when you just want `eth_getBalance` or `sentrix_getValidatorSet` without a full Web3 client.
3. **ethers.js v6** / **viem** — for wallet flows (MetaMask, `eth_sendRawTransaction`, contract calls). Sentrix is EIP-155 signed and revm-backed, so any standard Ethereum library works.

Everything below targets:
- Mainnet: `https://sentrix-rpc.sentriscloud.com` (chain_id 7119, PoA)
- Testnet: `https://testnet-rpc.sentriscloud.com` (chain_id 7120, BFT)

Unit reminder: REST returns **sentri** (1 SRX = 1e8 sentri). JSON-RPC / ethers / viem return **wei** (1 SRX = 1e18 wei). Convert accordingly.

---

## 1. Plain `fetch` — no dependencies

### Constants
```ts
// src/sentrix/constants.ts
export const SENTRIX = {
  mainnet: {
    rpc: "https://sentrix-rpc.sentriscloud.com",
    chainId: 7119,
    consensus: "PoA",
  },
  testnet: {
    rpc: "https://testnet-rpc.sentriscloud.com",
    chainId: 7120,
    consensus: "BFT",
  },
} as const;

export const SRX_DECIMALS = 8;            // native (sentri)
export const SRX_WEI_DECIMALS = 18;       // JSON-RPC surface (wei)
export const TOKEN_OP_ADDRESS = "0x0000000000000000000000000000000000000000";
```

### REST helpers
```ts
// src/sentrix/rest.ts
import { SENTRIX } from "./constants";

type Network = "mainnet" | "testnet";

export async function getChainInfo(network: Network = "mainnet") {
  const r = await fetch(`${SENTRIX[network].rpc}/chain/info`);
  return r.json() as Promise<{
    chain_id: number; height: number; total_blocks: number;
    active_validators: number; circulating_supply_srx: number;
    max_supply_srx: number; mempool_size: number; deployed_tokens: number;
  }>;
}

export async function getBalance(address: string, network: Network = "mainnet") {
  // REST → sentri
  const r = await fetch(`${SENTRIX[network].rpc}/accounts/${address}/balance`);
  if (r.status === 404) return 0n;
  const j = await r.json() as { balance: number };
  return BigInt(j.balance); // sentri
}

export async function getNonce(address: string, network: Network = "mainnet") {
  const r = await fetch(`${SENTRIX[network].rpc}/accounts/${address}/nonce`);
  const j = await r.json() as { nonce: number };
  return j.nonce;
}

export async function listTokens(network: Network = "mainnet") {
  const r = await fetch(`${SENTRIX[network].rpc}/tokens`);
  return r.json() as Promise<{
    tokens: Array<{
      contract_address: string; name: string; symbol: string;
      decimals: number; total_supply: number; max_supply: number;
      owner: string; holders: number;
    }>;
    total: number;
  }>;
}

export async function getTokenBalance(
  contract: string, address: string, network: Network = "mainnet",
) {
  const r = await fetch(
    `${SENTRIX[network].rpc}/tokens/${contract}/balance/${address}`,
  );
  const j = await r.json() as { balance: number };
  return BigInt(j.balance);
}

export async function getStatus(network: Network = "mainnet") {
  const r = await fetch(`${SENTRIX[network].rpc}/sentrix_status`);
  return r.json() as Promise<{
    version: { version: string; build: string };
    chain_id: number; consensus: "PoA" | "BFT"; native_token: "SRX";
    sync_info: {
      latest_block_height: number; latest_block_hash: string;
      latest_block_time: number; earliest_block_height: number;
      syncing: boolean;
    };
    validators: { active_count: number };
    uptime_seconds: number;
  }>;
}
```

### JSON-RPC helper
```ts
// src/sentrix/rpc.ts
import { SENTRIX } from "./constants";

type Network = "mainnet" | "testnet";

export async function rpc<T = unknown>(
  method: string, params: unknown[] = [], network: Network = "mainnet",
): Promise<T> {
  const r = await fetch(`${SENTRIX[network].rpc}/rpc`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", method, params, id: 1 }),
  });
  const j = await r.json() as {
    result?: T;
    error?: { code: number; message: string };
  };
  if (j.error) throw new Error(`${method} ${j.error.code}: ${j.error.message}`);
  return j.result as T;
}

// Typed wrappers
export async function getBlockNumber(network: Network = "mainnet"): Promise<bigint> {
  const hex = await rpc<string>("eth_blockNumber", [], network);
  return BigInt(hex);
}

export async function getBalanceWei(address: string, network: Network = "mainnet") {
  const hex = await rpc<string>("eth_getBalance", [address, "latest"], network);
  return BigInt(hex); // wei
}

export async function getValidatorSet(network: Network = "mainnet") {
  return rpc<{
    consensus: "PoA" | "DPoS";
    active_count: number; total_count: number;
    total_active_stake: string; epoch_number: number;
    validators: Array<{
      address: string; name: string; stake: string;
      commission: number; status: "active"|"jailed"|"tombstoned"|"unbonding";
      blocks_produced_epoch: number; uptime: number; voting_power: string;
    }>;
  }>("sentrix_getValidatorSet", [], network);
}
```

### Unit conversion helpers
```ts
// src/sentrix/units.ts
export const WEI_PER_SENTRI = 10_000_000_000n; // 1e10
export const SENTRI_PER_SRX = 100_000_000n;    // 1e8
export const WEI_PER_SRX    = 1_000_000_000_000_000_000n; // 1e18

export const weiToSentri = (wei: bigint) => wei / WEI_PER_SENTRI;
export const sentriToWei = (s: bigint) => s * WEI_PER_SENTRI;
export const weiToSrx    = (wei: bigint) => Number(wei) / 1e18;
export const srxToSentri = (srx: number) => BigInt(Math.floor(srx * 1e8));
export const srxToWei    = (srx: number) => BigInt(Math.floor(srx * 1e18));

// Format for display
export const formatSrx = (wei: bigint, digits = 4) => {
  const whole = wei / WEI_PER_SRX;
  const frac = (wei % WEI_PER_SRX).toString().padStart(18, "0").slice(0, digits);
  return `${whole}.${frac}`;
};
```

---

## 2. ethers.js v6 — wallet + contracts

Sentrix speaks EIP-155 signed transactions and implements the full `eth_*` namespace, so ethers just works. `eth_sendRawTransaction` is live (EVM enabled).

### Setup
```ts
// src/sentrix/ethers.ts
import { JsonRpcProvider, Wallet, formatEther, parseEther } from "ethers";
import { SENTRIX } from "./constants";

export const providerMainnet = new JsonRpcProvider(
  `${SENTRIX.mainnet.rpc}/rpc`, { chainId: 7119, name: "sentrix-mainnet" },
);
export const providerTestnet = new JsonRpcProvider(
  `${SENTRIX.testnet.rpc}/rpc`, { chainId: 7120, name: "sentrix-testnet" },
);
```

### MetaMask connect
```ts
// src/sentrix/metamask.ts
import { BrowserProvider } from "ethers";

// Register Sentrix mainnet with MetaMask
export async function addSentrixMainnet() {
  await window.ethereum.request({
    method: "wallet_addEthereumChain",
    params: [{
      chainId: "0x1bcf",             // 7119
      chainName: "Sentrix Mainnet",
      nativeCurrency: { name: "Sentrix", symbol: "SRX", decimals: 18 },
      rpcUrls: ["https://sentrix-rpc.sentriscloud.com/rpc"],
      blockExplorerUrls: ["https://sentrixscan.sentriscloud.com"],
    }],
  });
}

// Testnet
export async function addSentrixTestnet() {
  await window.ethereum.request({
    method: "wallet_addEthereumChain",
    params: [{
      chainId: "0x1bd0",             // 7120
      chainName: "Sentrix Testnet",
      nativeCurrency: { name: "Sentrix", symbol: "SRX", decimals: 18 },
      rpcUrls: ["https://testnet-rpc.sentriscloud.com/rpc"],
      blockExplorerUrls: ["https://testnet-scan.sentriscloud.com"],
    }],
  });
}

export async function connect() {
  const provider = new BrowserProvider(window.ethereum);
  const accounts = await provider.send("eth_requestAccounts", []);
  const signer = await provider.getSigner();
  return { provider, signer, address: accounts[0] };
}
```

### Send SRX
```ts
import { parseEther, formatEther } from "ethers";
import { connect } from "./metamask";

export async function sendSRX(to: string, amountSRX: string) {
  const { signer } = await connect();
  const tx = await signer.sendTransaction({
    to,
    value: parseEther(amountSRX),  // SRX → wei
  });
  return tx.wait();
}
```

### ERC-20 / SRC-20 (EVM path)
If the token was deployed via Solidity (`eth_sendRawTransaction`), treat it as a normal ERC-20:
```ts
import { Contract, formatUnits, parseUnits } from "ethers";
import { connect } from "./metamask";

const ERC20_ABI = [
  "function name() view returns (string)",
  "function symbol() view returns (string)",
  "function decimals() view returns (uint8)",
  "function totalSupply() view returns (uint256)",
  "function balanceOf(address) view returns (uint256)",
  "function transfer(address,uint256) returns (bool)",
];

export async function tokenBalance(tokenAddress: string, holder: string) {
  const { provider } = await connect();
  const c = new Contract(tokenAddress, ERC20_ABI, provider);
  const [bal, dec] = await Promise.all([c.balanceOf(holder), c.decimals()]);
  return formatUnits(bal, dec);
}

export async function tokenTransfer(
  tokenAddress: string, to: string, amount: string,
) {
  const { signer } = await connect();
  const c = new Contract(tokenAddress, ERC20_ABI, signer);
  const dec = await c.decimals();
  const tx = await c.transfer(to, parseUnits(amount, dec));
  return tx.wait();
}
```

### Native SRC-20 (TokenOp path)
Native-layer tokens live at `SRC20_<40 hex>` addresses (not Ethereum-style), so ethers can't address them directly. Use REST:
```ts
import { listTokens, getTokenBalance } from "./rest";

const mySrcTokens = await listTokens();
// → filter for SRC20_ addresses
const balance = await getTokenBalance(
  "SRC20_df98a9e4407bc2d28cd7e9046698e2d1cb0834ae",
  "0x682126...",
);
```

For transfers use `POST /tokens/{contract}/transfer` with a pre-signed transaction — see the signing recipe in `API_ENDPOINTS.md`.

---

## 3. viem (modern alternative)

```ts
// src/sentrix/viem.ts
import {
  createPublicClient, createWalletClient, custom, http, parseEther,
  defineChain,
} from "viem";

export const sentrixMainnet = defineChain({
  id: 7119,
  name: "Sentrix Mainnet",
  nativeCurrency: { name: "Sentrix", symbol: "SRX", decimals: 18 },
  rpcUrls: {
    default: { http: ["https://sentrix-rpc.sentriscloud.com/rpc"] },
  },
  blockExplorers: {
    default: {
      name: "Sentrix Scan",
      url: "https://sentrixscan.sentriscloud.com",
    },
  },
});

export const sentrixTestnet = defineChain({
  id: 7120,
  name: "Sentrix Testnet",
  nativeCurrency: { name: "Sentrix", symbol: "SRX", decimals: 18 },
  rpcUrls: {
    default: { http: ["https://testnet-rpc.sentriscloud.com/rpc"] },
  },
});

export const publicClient = createPublicClient({
  chain: sentrixMainnet,
  transport: http(),
});

// Example: latest block
const block = await publicClient.getBlock();
console.log(block.number, block.hash);

// Balance
const wei = await publicClient.getBalance({ address: "0x682126..." });

// Wallet (with MetaMask)
export const walletClient = createWalletClient({
  chain: sentrixMainnet,
  transport: custom(window.ethereum),
});
const [account] = await walletClient.requestAddresses();
const hash = await walletClient.sendTransaction({
  account, to: "0x...", value: parseEther("1.5"),
});
```

---

## 4. Polling patterns (no WebSocket yet)

`eth_subscribe` is queued as backlog #5/#6 (WebSocket RPC). Until then, poll. A 1s interval matches the block cadence.

```ts
// Poll latest block
export function watchBlocks(onBlock: (height: number) => void, intervalMs = 1000) {
  let last = 0;
  const timer = setInterval(async () => {
    try {
      const height = Number(await getBlockNumber());
      if (height > last) {
        last = height;
        onBlock(height);
      }
    } catch (_) { /* swallow, retry next tick */ }
  }, intervalMs);
  return () => clearInterval(timer);
}

// Poll a tx until mined
export async function waitForTx(
  txid: string, network: "mainnet"|"testnet" = "mainnet", timeoutMs = 30_000,
) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await fetch(`${SENTRIX[network].rpc}/transactions/${txid}`);
    if (r.ok) return r.json();
    await new Promise(res => setTimeout(res, 1000));
  }
  throw new Error(`tx ${txid} not mined within ${timeoutMs}ms`);
}
```

---

## 5. Error handling

All JSON-RPC calls return `{ error: { code, message } }` on failure. The `rpc()` helper above throws on error. REST returns 400/404/429 status codes.

```ts
try {
  await rpc("eth_getBalance", ["not-an-address", "latest"]);
} catch (e) {
  console.error(e); // "eth_getBalance -32602: address must be 42 chars (0x + 40 hex)"
}
```

Handle rate limits (429) with backoff:
```ts
export async function fetchWithRetry(url: string, opts?: RequestInit, tries = 3) {
  for (let i = 0; i < tries; i++) {
    const r = await fetch(url, opts);
    if (r.status !== 429) return r;
    const retryMs = 60_000 * Math.pow(2, i); // 60s, 120s, 240s
    await new Promise(res => setTimeout(res, retryMs));
  }
  throw new Error(`${url} still rate-limited after ${tries} tries`);
}
```

---

## 6. React hooks (optional)

```tsx
// useBalance.ts
import { useEffect, useState } from "react";
import { getBalance } from "./rest";

export function useBalance(address: string | null, intervalMs = 3000) {
  const [balance, setBalance] = useState<bigint | null>(null);
  useEffect(() => {
    if (!address) return;
    let cancelled = false;
    const tick = async () => {
      const b = await getBalance(address);
      if (!cancelled) setBalance(b);
    };
    tick();
    const t = setInterval(tick, intervalMs);
    return () => { cancelled = true; clearInterval(t); };
  }, [address, intervalMs]);
  return balance; // sentri
}
```

---

## 7. package.json deps

For full Web3 flows:
```json
{
  "dependencies": {
    "ethers": "^6.15.0"
  }
}
```

or viem:
```json
{
  "dependencies": {
    "viem": "^2.33.0"
  }
}
```

For REST-only integration: no deps needed (use native `fetch`).

---

## 8. Common gotchas

- **sentri vs wei.** REST returns sentri, JSON-RPC returns wei. Convert via `WEI_PER_SENTRI = 1e10`.
- **SRC-20 native addresses (`SRC20_...`) are not EVM addresses.** Don't pass them into ethers `Contract` — use REST endpoints.
- **Block tag `latest`** resolves to chain height at RPC dispatch time; two successive calls can return different blocks. Pin `blockTag: <hex height>` for consistent snapshots.
- **Rate limits per IP.** Frontend on a shared proxy (Cloudflare, Vercel) may share limits with other tenants. If you're on a dedicated edge, 60 req/min / 10 req/min is plenty.
- **CORS.** Sentrix RPC `SENTRIX_CORS_ORIGIN` is configurable per validator. Mainnet/testnet live behind Caddy which sets permissive CORS for known frontend origins. If you see a CORS error, check with the chain operator.
- **No WebSocket.** Poll or wait for backlog #5/#6.

---

## 9. Flutter / Dart (mobile APK)

For `sentrix-wallet-mobile` and any Dart-side integrator. Same pattern as JS/TS, different syntax.

### pubspec.yaml
```yaml
dependencies:
  flutter:
    sdk: flutter
  http: ^1.5.0
  web3dart: ^3.0.0          # EVM client — eth_* + signing
  convert: ^3.1.1           # hex utils
  pointycastle: ^3.9.0      # secp256k1, sha256 (for native signing)
  flutter_secure_storage: ^9.2.2  # keystore persistence
```

### Constants
```dart
// lib/sentrix/constants.dart
class Sentrix {
  static const mainnetRpc = 'https://sentrix-rpc.sentriscloud.com';
  static const testnetRpc = 'https://testnet-rpc.sentriscloud.com';
  static const mainnetChainId = 7119;
  static const testnetChainId = 7120;
  static const tokenOpAddress = '0x0000000000000000000000000000000000000000';

  // Unit scale
  static const weiPerSentri = 10000000000;       // 1e10
  static const sentriPerSrx = 100000000;          // 1e8
}
```

### REST helpers
```dart
// lib/sentrix/rest.dart
import 'dart:convert';
import 'package:http/http.dart' as http;
import 'constants.dart';

class SentrixRest {
  final String baseUrl;
  SentrixRest({this.baseUrl = Sentrix.mainnetRpc});

  factory SentrixRest.testnet() => SentrixRest(baseUrl: Sentrix.testnetRpc);

  Future<Map<String, dynamic>> chainInfo() async {
    final r = await http.get(Uri.parse('$baseUrl/chain/info'));
    return jsonDecode(r.body) as Map<String, dynamic>;
  }

  Future<Map<String, dynamic>> status() async {
    final r = await http.get(Uri.parse('$baseUrl/sentrix_status'));
    return jsonDecode(r.body) as Map<String, dynamic>;
  }

  /// Returns balance in sentri (1 SRX = 1e8 sentri).
  Future<BigInt> balance(String address) async {
    final r = await http.get(
      Uri.parse('$baseUrl/accounts/$address/balance'),
    );
    if (r.statusCode == 404) return BigInt.zero;
    final j = jsonDecode(r.body) as Map<String, dynamic>;
    return BigInt.from(j['balance'] as int);
  }

  Future<int> nonce(String address) async {
    final r = await http.get(Uri.parse('$baseUrl/accounts/$address/nonce'));
    final j = jsonDecode(r.body) as Map<String, dynamic>;
    return j['nonce'] as int;
  }

  Future<List<dynamic>> tokens() async {
    final r = await http.get(Uri.parse('$baseUrl/tokens'));
    final j = jsonDecode(r.body) as Map<String, dynamic>;
    return j['tokens'] as List<dynamic>;
  }

  Future<BigInt> tokenBalance(String contract, String holder) async {
    final r = await http.get(
      Uri.parse('$baseUrl/tokens/$contract/balance/$holder'),
    );
    final j = jsonDecode(r.body) as Map<String, dynamic>;
    return BigInt.from(j['balance'] as int);
  }

  Future<Map<String, dynamic>?> transaction(String txid) async {
    final r = await http.get(Uri.parse('$baseUrl/transactions/$txid'));
    if (r.statusCode == 404) return null;
    return jsonDecode(r.body) as Map<String, dynamic>;
  }

  Future<Map<String, dynamic>> mempool() async {
    final r = await http.get(Uri.parse('$baseUrl/mempool'));
    return jsonDecode(r.body) as Map<String, dynamic>;
  }
}
```

### JSON-RPC wrapper
```dart
// lib/sentrix/rpc.dart
import 'dart:convert';
import 'package:http/http.dart' as http;

class SentrixRpc {
  final String baseUrl;
  SentrixRpc({required this.baseUrl});

  Future<dynamic> call(String method, [List<dynamic> params = const []]) async {
    final r = await http.post(
      Uri.parse('$baseUrl/rpc'),
      headers: {'Content-Type': 'application/json'},
      body: jsonEncode({
        'jsonrpc': '2.0',
        'method': method,
        'params': params,
        'id': 1,
      }),
    );
    final j = jsonDecode(r.body) as Map<String, dynamic>;
    if (j['error'] != null) {
      final err = j['error'] as Map<String, dynamic>;
      throw Exception('$method ${err['code']}: ${err['message']}');
    }
    return j['result'];
  }

  Future<BigInt> getBalance(String address) async {
    final hex = await call('eth_getBalance', [address, 'latest']) as String;
    return BigInt.parse(hex.substring(2), radix: 16); // wei
  }

  Future<int> getBlockNumber() async {
    final hex = await call('eth_blockNumber') as String;
    return int.parse(hex.substring(2), radix: 16);
  }

  Future<Map<String, dynamic>> getValidatorSet() async {
    return await call('sentrix_getValidatorSet') as Map<String, dynamic>;
  }
}
```

### Send SRX via web3dart (EVM path)
`web3dart` speaks the `eth_*` namespace. Sentrix is EIP-155, so signing flows work as-is.

```dart
// lib/sentrix/wallet.dart
import 'package:http/http.dart' as http;
import 'package:web3dart/web3dart.dart';
import 'constants.dart';

class SentrixWallet {
  final Web3Client client;
  final Credentials credentials;
  final int chainId;

  SentrixWallet({
    required String rpcUrl,
    required this.credentials,
    required this.chainId,
  }) : client = Web3Client(rpcUrl, http.Client());

  factory SentrixWallet.fromPrivateKey(String hex, {bool testnet = false}) {
    final creds = EthPrivateKey.fromHex(hex);
    return SentrixWallet(
      rpcUrl: '${testnet ? Sentrix.testnetRpc : Sentrix.mainnetRpc}/rpc',
      credentials: creds,
      chainId: testnet ? Sentrix.testnetChainId : Sentrix.mainnetChainId,
    );
  }

  Future<EthereumAddress> address() => credentials.extractAddress();

  /// Balance in wei.
  Future<EtherAmount> balance() async {
    return client.getBalance(await address());
  }

  /// Send SRX. `amountSrx` is a string like "1.5".
  Future<String> sendSrx(String to, String amountSrx) async {
    final amount = EtherAmount.fromBase10String(EtherUnit.ether, amountSrx);
    return client.sendTransaction(
      credentials,
      Transaction(
        to: EthereumAddress.fromHex(to),
        value: amount,
        gasPrice: EtherAmount.inWei(BigInt.from(1000000000)), // 1 Gwei
        maxGas: 21000,
      ),
      chainId: chainId,
    );
  }

  void dispose() => client.dispose();
}
```

### EVM contract (ERC-20 / SRC-20 via Solidity)
Same ABI as standard ERC-20. `web3dart` reads contract events + calls.

```dart
import 'package:web3dart/web3dart.dart';

const erc20Abi = '''[
  {"constant":true,"inputs":[{"name":"_owner","type":"address"}],
   "name":"balanceOf","outputs":[{"name":"balance","type":"uint256"}],"type":"function"},
  {"constant":true,"inputs":[],"name":"decimals",
   "outputs":[{"name":"","type":"uint8"}],"type":"function"},
  {"constant":false,"inputs":[{"name":"_to","type":"address"},
                              {"name":"_value","type":"uint256"}],
   "name":"transfer","outputs":[{"name":"","type":"bool"}],"type":"function"}
]''';

Future<BigInt> erc20Balance(
  SentrixWallet wallet, String tokenAddr, String holder,
) async {
  final contract = DeployedContract(
    ContractAbi.fromJson(erc20Abi, 'ERC20'),
    EthereumAddress.fromHex(tokenAddr),
  );
  final result = await wallet.client.call(
    contract: contract,
    function: contract.function('balanceOf'),
    params: [EthereumAddress.fromHex(holder)],
  );
  return result.first as BigInt;
}
```

### Native SRC-20 (TokenOp path)
Native SRC-20 tokens (addresses `SRC20_<40 hex>`) aren't EVM — use REST. Signing the TokenOp tx requires manual secp256k1 + canonical payload hashing. Reference Rust implementation: `crates/sentrix-primitives/src/transaction.rs::Transaction::new`. Dart port lives in `lib/sentrix/native_tx.dart` (TBD in sentrix-wallet-mobile repo).

For read-only (list tokens, balances, trades): use `SentrixRest` above.

### Secure key storage
Never put a private key in plain `SharedPreferences`. Use `flutter_secure_storage` which backs keystore/keychain on Android/iOS.

```dart
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

const _storage = FlutterSecureStorage(
  aOptions: AndroidOptions(encryptedSharedPreferences: true),
);

Future<void> saveKey(String privateKeyHex, {String label = 'default'}) =>
    _storage.write(key: 'sentrix_key_$label', value: privateKeyHex);

Future<String?> loadKey({String label = 'default'}) =>
    _storage.read(key: 'sentrix_key_$label');

Future<void> deleteKey({String label = 'default'}) =>
    _storage.delete(key: 'sentrix_key_$label');
```

### Polling pattern
```dart
import 'dart:async';

StreamSubscription<int> watchBlocks(
  SentrixRpc rpc, void Function(int) onBlock,
  {Duration interval = const Duration(seconds: 1)}
) {
  int last = 0;
  final controller = StreamController<int>();
  final timer = Timer.periodic(interval, (_) async {
    try {
      final h = await rpc.getBlockNumber();
      if (h > last) {
        last = h;
        controller.add(h);
      }
    } catch (_) { /* retry next tick */ }
  });
  controller.onCancel = () => timer.cancel();
  return controller.stream.listen(onBlock);
}
```

### Riverpod / Provider pattern
```dart
// Example with Riverpod — swap for Provider/Bloc as you prefer.
final sentrixRestProvider = Provider((ref) => SentrixRest());
final sentrixRpcProvider  = Provider((ref) =>
    SentrixRpc(baseUrl: Sentrix.mainnetRpc));

final balanceProvider = FutureProvider.family<BigInt, String>((ref, address) {
  return ref.watch(sentrixRestProvider).balance(address);
});

// In a widget:
final balance = ref.watch(balanceProvider(userAddress));
balance.when(
  data: (sentri) => Text('${sentri / BigInt.from(1e8)} SRX'),
  loading: () => const CircularProgressIndicator(),
  error: (e, _) => Text('Error: $e'),
);
```
