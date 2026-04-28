# Contract Verification (Sourcify)

Verify your Solidity contract source code against on-chain bytecode using Sentrix's self-hosted Sourcify instance.

**Endpoint:** `https://verify.sentrixchain.com`
**Supported chains:** mainnet (7119), testnet (7120)

## Why verify

Once verified, your contract:
- Shows a green ✓ "verified" badge on [scan.sentrixchain.com](https://scan.sentrixchain.com) address pages
- Returns matched source + ABI + metadata via API (any tool can fetch your code)
- Independently re-compilable by auditors / users (transparency)
- Required for many listing platforms + due-diligence reviews

## Quick reference: existing verified contracts

All 4 canonical contracts on both chains are already verified:

| Contract | Mainnet (7119) | Testnet (7120) |
|---|---|---|
| WSRX | [`0x4693b113...`](https://verify.sentrixchain.com/files/any/7119/0x4693b113e523A196d9579333c4ab8358e2656553) | [`0x85d5E769...`](https://verify.sentrixchain.com/files/any/7120/0x85d5E7694AF31C2Edd0a7e66b7c6c92C59fF949A) |
| Multicall3 | [`0xFd4b34b5...`](https://verify.sentrixchain.com/files/any/7119/0xFd4b34b5763f54a580a0d9f7997A2A993ef9ceE9) | [`0x7900826D...`](https://verify.sentrixchain.com/files/any/7120/0x7900826De548425c6BE56caEbD4760AB0155Cd54) |
| TokenFactory | [`0xc753199b...`](https://verify.sentrixchain.com/files/any/7119/0xc753199b723649ab92c6db8A45F158921CFDEe49) | [`0x7A2992af...`](https://verify.sentrixchain.com/files/any/7120/0x7A2992af0d4979aDD076347666023d66d29276Fc) |
| SentrixSafe | [`0x6272dC0C...`](https://verify.sentrixchain.com/files/any/7119/0x6272dC0C842F05542f9fF7B5443E93C0642a3b26) | [`0xc9D7a61D...`](https://verify.sentrixchain.com/files/any/7120/0xc9D7a61D7C2F428F6A055916488041fD00532110) |

## How to verify your own contract

### Prerequisites

You need:
- The deployed contract address
- The Solidity source file (`.sol`)
- The compiler-emitted `metadata.json`

### Getting `metadata.json`

#### Foundry / forge

After `forge build`, find it under `out/<ContractName>.sol/<ContractName>.json` — the build artifact contains a `rawMetadata` field which IS the metadata.json content. Extract:

```bash
jq -r '.rawMetadata' out/MyToken.sol/MyToken.json > metadata.json
```

#### Hardhat

After `npx hardhat compile`, find it under `artifacts/build-info/<hash>.json` — extract the metadata for your contract:

```bash
jq -r '.output.contracts["contracts/MyToken.sol"]["MyToken"].metadata' artifacts/build-info/*.json > metadata.json
```

Or use the Hardhat verify plugin (if a Sourcify-compatible one exists; check the plugin docs).

#### Remix

In Remix, after compile:
- Go to **Solidity Compiler** tab → **Compilation Details** (top-right of compile output)
- Click **METADATA** → copy the JSON
- Save as `metadata.json`

### Submit verification (curl)

```bash
curl -X POST https://verify.sentrixchain.com/verify \
  -H "Content-Type: application/json" \
  -d '{
    "address": "0xYourContractAddress",
    "chain": "7119",
    "files": {
      "MyToken.sol": "<paste full source code here>",
      "metadata.json": "<paste full metadata.json here>"
    }
  }'
```

Use `"chain": "7120"` for testnet.

#### Multi-file contracts

If your contract imports other contracts, include each file with the path used in the import:

```bash
curl -X POST https://verify.sentrixchain.com/verify \
  -H "Content-Type: application/json" \
  -d '{
    "address": "0xYourContractAddress",
    "chain": "7119",
    "files": {
      "src/MyToken.sol": "<source>",
      "src/IERC20.sol": "<imported source>",
      "lib/openzeppelin/Ownable.sol": "<another import>",
      "metadata.json": "<metadata json>"
    }
  }'
```

The metadata.json has the canonical filenames in its `sources` field — match those exactly.

### Submit verification (Python)

```python
import json, urllib.request

with open("MyToken.sol") as f:
    source = f.read()
with open("metadata.json") as f:
    metadata = f.read()

payload = {
    "address": "0xYourContractAddress",
    "chain": "7119",  # or "7120" for testnet
    "files": {
        "MyToken.sol": source,
        "metadata.json": metadata,
    },
}

req = urllib.request.Request(
    "https://verify.sentrixchain.com/verify",
    data=json.dumps(payload).encode(),
    headers={"Content-Type": "application/json"},
    method="POST",
)
resp = urllib.request.urlopen(req, timeout=120)
print(resp.read().decode())
```

### Response

Success:
```json
{
  "result": [
    {
      "address": "0x...",
      "chainId": "7119",
      "status": "perfect"
    }
  ]
}
```

Status values:
- **`perfect`** — bytecode + metadata exact match (green badge)
- **`partial`** — bytecode matches but metadata differs (amber badge — usually means compiler optimization mismatch)
- HTTP 500 with error: verification failed (bytecode mismatch, source doesn't compile to deployed bytecode, RPC fetch failed, etc.)

## Check verification status

### Via API

```bash
curl https://verify.sentrixchain.com/files/any/7119/0xYourContractAddress
```

Returns JSON with `status` (`full`/`partial`) and the verified files. 404 if not verified.

### Via scan UI

Visit https://scan.sentrixchain.com/en/address/0xYourContractAddress — verification badge appears in the header next to the address label.

## List supported chains

```bash
curl https://verify.sentrixchain.com/chains
```

Currently returns chains 7119 (mainnet) and 7120 (testnet). If your chain isn't listed, this is a self-hosted instance scoped to Sentrix only — for other chains use [sourcify.dev](https://sourcify.dev).

## Common verification failures

| Error | Cause | Fix |
|---|---|---|
| "Cannot fetch bytecode" | Sourcify can't reach the RPC, or the address has no bytecode (EOA) | Verify address is a contract via `eth_getCode` first |
| "perfect" not achieved, only "partial" | Compiler version or settings differ from deployment | Use the EXACT solc version + optimization runs that were used to deploy |
| "Source code does not match" | Source file modified after deploy | Use the exact source that was compiled + deployed |
| HTTP 500 with cryptic error | Sourcify internal issue | Retry after a few seconds; if persistent, file an issue at [sentrix-labs/sentrix](https://github.com/sentrix-labs/sentrix) |

## Self-hosted vs public Sourcify

- **Self-hosted (`verify.sentrixchain.com`):** Sentrix-specific. Supports chain 7119 + 7120. All canonical Sentrix contracts verified here.
- **Public ([`sourcify.dev`](https://sourcify.dev)):** Doesn't currently support Sentrix chains. If/when Sourcify adds Sentrix to their supported list, the public instance will work too — for now, use ours.

## Privacy / security

- Verification is **public by design** — uploaded source code becomes publicly readable via the API.
- Do **NOT** upload contracts you want to keep private (e.g., proprietary / not-yet-released code).
- The API has no authentication — anyone can verify any contract address (this is intentional; Sourcify model).
