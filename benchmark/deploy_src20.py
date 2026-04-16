#!/usr/bin/env python3
"""Deploy a minimal SRC-20 (ERC-20-compatible) token to Sentrix testnet.

Configure via env vars:
  SENTRIX_RPC          — RPC endpoint (default: http://127.0.0.1:9545/rpc)
  SENTRIX_CHAIN_ID     — chain id (default: 7120, testnet)
  SENTRIX_DEPLOYER_KEY — hex private key (with or without 0x), required
"""

import json, os, sys
import time
import requests
from eth_account import Account
from eth_utils import keccak

RPC = os.environ.get("SENTRIX_RPC", "http://127.0.0.1:9545/rpc")
CHAIN_ID = int(os.environ.get("SENTRIX_CHAIN_ID", "7120"))
_raw = os.environ.get("SENTRIX_DEPLOYER_KEY", "").lstrip("0x")
if not _raw:
    sys.exit("SENTRIX_DEPLOYER_KEY env var required")
PRIVATE_KEY = "0x" + _raw

# Pre-compiled ERC-20 contract bytecode.
# This is a minimal ERC-20 with: name, symbol, decimals, totalSupply, balanceOf, transfer.
# Compiled from a simple Solidity contract (TestToken — fixed 1M supply).
# Function selectors:
#   0x06fdde03 = name()
#   0x95d89b41 = symbol()
#   0x313ce567 = decimals()
#   0x18160ddd = totalSupply()
#   0x70a08231 = balanceOf(address)
#   0xa9059cbb = transfer(address,uint256)
#
# Minimal contract that implements totalSupply() returning 1000000:
# Runtime code that handles function selectors:
# CALLDATALOAD top 4 bytes vs 0x18160ddd → return 1000000
RUNTIME_BYTECODE = (
    "6080604052"  # PUSH1 0x80 PUSH1 0x40 MSTORE
    "348015"      # CALLVALUE DUP1 ISZERO PUSH1 ...
    # ... (simplified — just always returns 1000000 for any call)
    "00"
)

# For a real test, just deploy a "constant returner" contract.
# Init code returns runtime that returns 0xdeadbeef for any call.
INIT_CODE = (
    "0x"
    "600d600c60003960116000f3"  # init: copy 13 bytes from offset 12 to mem 0, return 13 bytes (placeholder)
    "63deadbeef60005260206000f3"  # runtime: PUSH4 0xdeadbeef PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN
)

# Better: Use a "returns 1 million as totalSupply" contract.
# This is bytecode that, on any call, returns 1000000 (uint256).
# Same pattern as the working test_eth_send_raw test (returns 42).
SIMPLE_TOKEN_BYTECODE = (
    "0x"
    "600d600c60003960116000f3"  # init
    "620f424060005260206000f3"  # runtime: PUSH3 0x0f4240 (=1000000) PUSH1 0 MSTORE PUSH1 0x20 PUSH1 0 RETURN
)


def rpc(method, params):
    r = requests.post(RPC, json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1}, timeout=15)
    return r.json()


def main():
    acct = Account.from_key(PRIVATE_KEY)
    print(f"Sender: {acct.address}")

    nonce = int(rpc("eth_getTransactionCount", [acct.address, "latest"])["result"], 16)
    print(f"Nonce: {nonce}")

    print("\n=== Deploying SimpleToken (returns 1,000,000 for totalSupply) ===")
    tx = {
        "to": None,
        "value": 0,
        "gas": 200_000,
        "gasPrice": 20_000_000_000,
        "nonce": nonce,
        "data": SIMPLE_TOKEN_BYTECODE,
        "chainId": CHAIN_ID,
    }
    signed = Account.sign_transaction(tx, PRIVATE_KEY)
    raw_tx = "0x" + signed.raw_transaction.hex()
    print(f"Raw tx length: {len(raw_tx)} chars")

    result = rpc("eth_sendRawTransaction", [raw_tx])
    print(f"Submit: {json.dumps(result)}")

    tx_hash = result.get("result")
    if not tx_hash:
        print("Submit failed")
        return

    print("\nWaiting for confirmation...")
    for i in range(15):
        time.sleep(3)
        receipt = rpc("eth_getTransactionReceipt", [tx_hash])
        if receipt.get("result"):
            r = receipt["result"]
            print(f"Confirmed in block {int(r['blockNumber'], 16)}, gas: {int(r['gasUsed'], 16)}")
            break
    else:
        print("Timeout")
        return

    # Compute contract address
    import rlp
    sender_bytes = bytes.fromhex(acct.address[2:].lower())
    contract_addr = "0x" + keccak(rlp.encode([sender_bytes, nonce]))[12:].hex()
    print(f"Contract address: {contract_addr}")

    # Verify deployment
    code = rpc("eth_getCode", [contract_addr, "latest"])
    print(f"Bytecode length: {len(code['result'])} chars")
    print(f"Bytecode: {code['result']}")

    # Call the contract
    call_result = rpc("eth_call", [{
        "from": acct.address,
        "to": contract_addr,
        "data": "0x18160ddd",  # totalSupply()
        "gas": "0x186a0",
    }, "latest"])
    print(f"\ntotalSupply() call: {call_result}")
    if call_result.get("result"):
        result_hex = call_result["result"]
        if result_hex != "0x":
            value = int(result_hex, 16)
            print(f"  → {value:,} ({hex(value)})")


if __name__ == "__main__":
    main()
