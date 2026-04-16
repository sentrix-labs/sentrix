#!/usr/bin/env python3
"""Deploy a minimal Solidity contract to Sentrix testnet via EVM.

Configure via env vars:
  SENTRIX_RPC          — RPC endpoint (default: http://127.0.0.1:9545/rpc)
  SENTRIX_CHAIN_ID     — chain id (default: 7120, testnet)
  SENTRIX_DEPLOYER_KEY — raw hex private key (no 0x), required
"""

import hashlib, json, os, requests, sys, time
from collections import OrderedDict
from ecdsa import SigningKey, SECP256k1
from ecdsa.util import sigencode_string
from Crypto.Hash import keccak

RPC = os.environ.get("SENTRIX_RPC", "http://127.0.0.1:9545/rpc")
CHAIN_ID = int(os.environ.get("SENTRIX_CHAIN_ID", "7120"))
PRIVATE_KEY = os.environ.get("SENTRIX_DEPLOYER_KEY", "").lstrip("0x")
if not PRIVATE_KEY:
    sys.exit("SENTRIX_DEPLOYER_KEY env var required (raw hex, no 0x prefix)")

# Minimal contract: storage[0] = 42, returns 32 bytes
# PUSH1 0x42 PUSH1 0x00 SSTORE PUSH1 0x42 PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN
# As init code that deploys a trivial runtime:
# Init: copy 10-byte runtime to mem and return
# Runtime (10 bytes): 602a60005260206000f3 (returns 42 as uint256)
BYTECODE_HEX = "600a600c60003960096000f3" + "602a60005260206000f3"


def address_from_key(priv_hex):
    sk = SigningKey.from_string(bytes.fromhex(priv_hex), curve=SECP256k1)
    pk = sk.get_verifying_key().to_string()
    k = keccak.new(digest_bits=256)
    k.update(pk)
    return "0x" + k.hexdigest()[-40:], sk, "04" + pk.hex()


def rpc_call(method, params):
    r = requests.post(RPC, json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1}, timeout=15)
    return r.json()


def main():
    addr, sk, pubkey = address_from_key(PRIVATE_KEY)
    print(f"Sender: {addr}")

    # Check chain state
    chainid = rpc_call("eth_chainId", [])
    height = rpc_call("eth_blockNumber", [])
    balance = rpc_call("eth_getBalance", [addr, "latest"])
    print(f"chain_id: {chainid['result']}")
    print(f"height:   {height['result']} ({int(height['result'], 16)})")
    print(f"balance:  {balance.get('result', 'N/A')}")

    # Test eth_call on the zero address (should return empty)
    code = rpc_call("eth_getCode", ["0x0000000000000000000000000000000000000000", "latest"])
    print(f"getCode(0x0): {code['result']}")

    # Test eth_estimateGas for contract deploy
    est = rpc_call("eth_estimateGas", [{"from": addr, "data": "0x" + BYTECODE_HEX}])
    print(f"estimateGas: {est}")

    # Test eth_call with contract bytecode (should execute)
    # This is the runtime bytecode that returns 42
    runtime = "602a60005260206000f3"
    call_result = rpc_call("eth_call", [{"from": addr, "to": "0x0000000000000000000000000000000000000000", "data": "0x" + runtime, "gas": "0x186a0"}, "latest"])
    print(f"eth_call with runtime bytecode: {call_result}")


if __name__ == "__main__":
    main()
