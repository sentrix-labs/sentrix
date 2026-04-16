#!/usr/bin/env python3
"""Test eth_sendRawTransaction on Sentrix testnet.

Configure via env vars:
  SENTRIX_RPC          — RPC endpoint (default: http://127.0.0.1:9545/rpc)
  SENTRIX_CHAIN_ID     — chain id (default: 7120, testnet)
  SENTRIX_DEPLOYER_KEY — hex private key (with or without 0x), required
"""

import json, os, sys
import requests
from eth_account import Account

RPC = os.environ.get("SENTRIX_RPC", "http://127.0.0.1:9545/rpc")
CHAIN_ID = int(os.environ.get("SENTRIX_CHAIN_ID", "7120"))
_raw = os.environ.get("SENTRIX_DEPLOYER_KEY", "").lstrip("0x")
if not _raw:
    sys.exit("SENTRIX_DEPLOYER_KEY env var required")
PRIVATE_KEY = "0x" + _raw


def rpc(method, params):
    r = requests.post(RPC, json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1}, timeout=15)
    return r.json()


def main():
    acct = Account.from_key(PRIVATE_KEY)
    print(f"Sender: {acct.address}")

    # Get nonce
    nonce_resp = rpc("eth_getTransactionCount", [acct.address, "latest"])
    print(f"Nonce response: {nonce_resp}")
    nonce = int(nonce_resp.get("result", "0x0"), 16) if nonce_resp.get("result") else 0
    print(f"Nonce: {nonce}")

    # Get chain ID
    chainid = rpc("eth_chainId", [])
    print(f"Chain ID: {chainid['result']}")

    # Test 1: Simple transfer
    print("\n=== Test 1: Simple transfer ===")
    tx = {
        "to": "0x0000000000000000000000000000000000000001",
        "value": 1000,  # 1000 wei = 0.0000001 sentri
        "gas": 21000,
        "gasPrice": 20_000_000_000,  # 20 gwei
        "nonce": nonce,
        "chainId": CHAIN_ID,
    }
    signed = Account.sign_transaction(tx, PRIVATE_KEY)
    raw_tx = "0x" + signed.raw_transaction.hex()
    print(f"Raw tx: {raw_tx[:60]}...")

    result = rpc("eth_sendRawTransaction", [raw_tx])
    print(f"Send result: {json.dumps(result)}")

    # Test 2: Contract deploy (minimal contract — returns 42)
    print("\n=== Test 2: Contract deploy ===")
    # Init code: deploys runtime that returns 42
    bytecode = "0x600a600c600039600a6000f3" + "602a60005260206000f3"
    tx2 = {
        "to": None,  # CREATE
        "value": 0,
        "gas": 100_000,
        "gasPrice": 20_000_000_000,
        "nonce": nonce + 1,
        "data": bytecode,
        "chainId": CHAIN_ID,
    }
    signed2 = Account.sign_transaction(tx2, PRIVATE_KEY)
    raw_tx2 = "0x" + signed2.raw_transaction.hex()
    print(f"Raw tx: {raw_tx2[:60]}...")

    result2 = rpc("eth_sendRawTransaction", [raw_tx2])
    print(f"Deploy result: {json.dumps(result2)}")

    # Wait for block + check receipt
    import time
    if result2.get("result"):
        tx_hash = result2["result"]
        print(f"\nWaiting for block confirmation...")
        for i in range(20):
            time.sleep(3)
            receipt = rpc("eth_getTransactionReceipt", [tx_hash])
            if receipt.get("result"):
                print(f"Receipt: {json.dumps(receipt['result'], indent=2)}")
                break
            print(f"  attempt {i+1}: still pending")
        else:
            print("Timeout waiting for receipt")


if __name__ == "__main__":
    main()
