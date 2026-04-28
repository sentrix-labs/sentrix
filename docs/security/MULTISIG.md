---
sidebar_position: 5
title: Multi-sig & Authority Wallet
---

# SentrixSafe & authority wallet model

How privileged actions on Sentrix Chain are governed: a minimal Gnosis Safe-derived multi-sig, currently 1-of-1 with a dedicated authority signer. The contract is shape-ready to expand to N-of-M when independent signers are recruited; no specific expansion timeline is committed.

## Architecture

- **SentrixSafe contract** at `0x6272dC0C842F05542f9fF7B5443E93C0642a3b26` (mainnet 7119) and `0xc9D7a61D7C2F428F6A055916488041fD00532110` (testnet 7120). Source in [`canonical-contracts`](https://github.com/sentrix-labs/canonical-contracts/blob/main/contracts/SentrixSafe.sol).
- **Authority signer** EOA `0xa25236925bc10954e0519731cc7ba97f4bb5714b` — pure-signing role, holds 0 SRX, sole owner of both Safes with threshold=1.
- **Bootstrap deployer** EOA `0x5acb04058fc4dfa258f29ce318282377cac176fd` — deployed all four canonical contracts (WSRX, Multicall3, TokenFactory, SentrixSafe) on 2026-04-27. **Retired from Safe ownership 2026-04-28.**

## Why decoupled signers

Pre-2026-04-28, the single Founder wallet (Founder v3, `0x5b5b06688dcd...`, 21M SRX premine) was the natural Safe-owner candidate. Decoupling that role from the premine wallet to a dedicated authority EOA gives:

1. **Clean role separation** — Founder v3 = pure premine holder (passive). Authority = pure signer (0 SRX). If either keystore leaks the blast radius is bounded.
2. **Easier rotation** — rotating the signing key doesn't move 21M SRX around.
3. **Audit trail clarity** — every Safe `execTransaction` is signed by the authority address; no confusion with founder personal-funds movements.

## Migration history (2026-04-28)

Both Safes started as 1-of-1 multisigs owned by the bootstrap deployer (set at construction time). Migration sequence:

| Step | Chain | Action | Tx | Block |
|---|---|---|---|---|
| 1 | testnet 7120 | `addOwner(authority, threshold=1)` | `0xb70a83eb416e2323aa8cc422d72fc89bd9a6f6e4338ce2b6bc8560a711d0c70f` | 881639 |
| 2 | mainnet 7119 | `addOwner(authority, threshold=1)` | `0xd17400c35f0716db7410384fd728ed3b02185bf861880aad7b44326ba7690b19` | 755821 |
| 3 | testnet 7120 | `removeOwner(deployer, threshold=1)` | `0xb0c69e89252c4e00b920600b2211f3857c07da0aa7f5c6719cc3dc8c42b6d728` | 884599 |
| 4 | mainnet 7119 | `removeOwner(deployer, threshold=1)` | `0x8e9ca8b4cbe0bac8332de225045b83059b3a05ea2748d58d61218c7598d1d6e0` | 757829 |

Final state both chains: **1-of-1 with authority sole owner, threshold=1, nonce=2.**

## Off-chain SafeTx construction

Both `addOwner` and `removeOwner` were built fully off-chain because `eth_call` and `eth_getStorageAt` were stubbed pre-PR-#389. SafeTx hash computation:

```
TX_TYPEHASH      = keccak256("SafeTx(address to,uint256 value,bytes data,uint256 operation,uint256 nonce)")
DOMAIN_SEPARATOR = keccak256(abi.encode(EIP712_DOMAIN_TYPEHASH, chainId, safe_addr))
structHash       = keccak256(abi.encode(TX_TYPEHASH, to, value, keccak(data), operation, nonce))
txHash           = keccak256(0x1901 || DOMAIN_SEPARATOR || structHash)
signature        = cast wallet sign --no-hash <txHash> --private-key <signer>
```

Submitted via `cast send <safe> "execTransaction(address,uint256,bytes,uint256,bytes)" ...` from the deployer (which still had ~0.5 SRX for gas). Verified each tx receipt: `status=0x1`, two events fired (`AddedOwner(addr)` / `RemovedOwner(addr)` + `ExecutionSuccess(safeTxHash)`).

## Verifying current state

After PR #389 + #391 deployed in v2.1.47 (eth_call wired with EIP-7825 gas cap):

```bash
cast call 0x6272dC0C842F05542f9fF7B5443E93C0642a3b26 "getOwners()(address[])" \
  --rpc-url https://rpc.sentrixchain.com
# → [0xa25236925bc10954e0519731cc7ba97f4bb5714b]

cast call 0x6272dC0C842F05542f9fF7B5443E93C0642a3b26 "getThreshold()(uint256)" \
  --rpc-url https://rpc.sentrixchain.com
# → 1
```

## Multi-sig expansion (no committed timeline)

The contract is shape-ready for N-of-M expansion. Whenever independent signers are recruited and onboarded, the expansion is a same-day on-chain operation:

```
addOwner(<new_signer>, threshold=1)   // repeat per signer; threshold stays 1 until last step
changeThreshold(<new_threshold>)      // raise quorum once full signer set is in place
```

No timeline is committed because committing to a specific quarter without recruited signers in pipeline would be performative — a multi-sig wallet with non-responsive co-signers is operationally worse than a working 1-of-1.

The contract surface (1-of-1 today, expansion-ready) is the right signal to listing platforms and partners: governance contract exists + ready to expand, not "we will definitely have N signers by date X."

## Key contract operations gated by Safe

- **`SentrixSafe.addOwner(address, uint256)`** — add a new signer + new threshold.
- **`SentrixSafe.removeOwner(address, uint256)`** — remove a signer + new threshold. Enforces `owners.length - 1 >= _threshold`.
- **`SentrixSafe.changeThreshold(uint256)`** — change quorum without changing owner set.
- **`SentrixSafe.execTransaction(...)`** — execute arbitrary (target, value, calldata) operations against any contract. Used for the addOwner/removeOwner self-calls + future governance txs.

The existing canonical contracts (WSRX, Multicall3, TokenFactory) have **no owner role** — they're immutable after deployment. Only SentrixSafe has owner-set governance. If future contracts gain owner roles (e.g., upgradeable proxies, pausable factories), `script/TransferOwnership.s.sol` will document the hand-off path.

## See also

- [`canonical-contracts/docs/ADDRESSES.md`](https://github.com/sentrix-labs/canonical-contracts/blob/main/docs/ADDRESSES.md) — deployed addresses + ownership migration tx hashes
- [`canonical-contracts/contracts/SentrixSafe.sol`](https://github.com/sentrix-labs/canonical-contracts/blob/main/contracts/SentrixSafe.sol) — contract source
- [Tokenomics > Governance](../tokenomics/OVERVIEW#8-governance) — broader governance roadmap
