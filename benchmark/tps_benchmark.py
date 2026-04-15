#!/usr/bin/env python3
"""
Sentrix TPS Benchmark — measures transaction throughput on testnet.

Usage:
    python tps_benchmark.py [--rpc URL] [--count N]

Requires: pip install requests secp256k1 (or ecdsa as fallback)
"""

import hashlib
import json
import time
import sys
import os
import uuid
import struct
from collections import OrderedDict

try:
    import requests
except ImportError:
    print("ERROR: pip install requests")
    sys.exit(1)

# ── Configuration ─────────────────────────────────────────────

RPC_URL = os.environ.get("SENTRIX_RPC", "http://VPS3_IP_REDACTED:9545")
CHAIN_ID = 7120  # testnet
MIN_FEE = 10_000  # 0.0001 SRX in sentri
SENTRI_PER_SRX = 100_000_000

# Test wallet — generate a fresh one or use one with balance
# For testnet, we need a funded wallet. Use the faucet or an existing key.
# This key is for TESTNET ONLY.
PRIVATE_KEY_HEX = os.environ.get("SENTRIX_BENCH_KEY", "")
API_KEY = os.environ.get("SENTRIX_API_KEY", "")


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def hex_encode(data: bytes) -> str:
    return data.hex()


class SimpleECDSA:
    """Minimal ECDSA signer using the ecdsa library (pure Python fallback)."""

    def __init__(self, private_key_hex: str):
        try:
            from ecdsa import SigningKey, SECP256k1
            self.sk = SigningKey.from_string(bytes.fromhex(private_key_hex), curve=SECP256k1)
            self.pk = self.sk.get_verifying_key()
            self.backend = "ecdsa"
        except ImportError:
            print("ERROR: pip install ecdsa")
            sys.exit(1)

    def public_key_uncompressed(self) -> str:
        # 04 + 64 bytes x + 64 bytes y = 130 hex chars
        return "04" + self.pk.to_string().hex()

    def address(self) -> str:
        from Crypto.Hash import keccak as keccak_mod
        # Keccak-256 (NOT SHA3-256) of uncompressed pubkey (without 04 prefix)
        k = keccak_mod.new(digest_bits=256)
        k.update(self.pk.to_string())
        return "0x" + k.hexdigest()[-40:]

    def sign(self, payload_bytes: bytes) -> str:
        from ecdsa.util import sigencode_string
        h = sha256(payload_bytes)
        sig = self.sk.sign_digest(h, sigencode=sigencode_string)
        return sig.hex()


def build_signing_payload(from_addr, to_addr, amount, fee, nonce, data, timestamp, chain_id):
    """Build canonical signing payload matching Rust's BTreeMap ordering."""
    payload = OrderedDict([
        ("amount", amount),
        ("chain_id", chain_id),
        ("data", data),
        ("fee", fee),
        ("from", from_addr),
        ("nonce", nonce),
        ("timestamp", timestamp),
        ("to", to_addr),
    ])
    return json.dumps(payload, separators=(",", ":"))


def create_signed_tx(signer, from_addr, to_addr, amount, fee, nonce, chain_id):
    """Create a properly signed Sentrix transaction."""
    timestamp = int(time.time())
    data = ""

    payload_str = build_signing_payload(
        from_addr, to_addr, amount, fee, nonce, data, timestamp, chain_id
    )

    signature = signer.sign(payload_str.encode("utf-8"))
    public_key = signer.public_key_uncompressed()

    # Compute txid = SHA-256(signing_payload) — must match Rust's compute_txid()
    txid = hashlib.sha256(payload_str.encode("utf-8")).hexdigest()

    return {
        "txid": txid,
        "from_address": from_addr,
        "to_address": to_addr,
        "amount": amount,
        "fee": fee,
        "nonce": nonce,
        "data": data,
        "timestamp": timestamp,
        "chain_id": chain_id,
        "signature": signature,
        "public_key": public_key,
    }


def get_chain_info(rpc_url):
    r = requests.get(f"{rpc_url}/chain/info", timeout=10)
    return r.json()


def get_nonce(rpc_url, address):
    r = requests.get(f"{rpc_url}/accounts/{address}/nonce", timeout=10)
    data = r.json()
    return data.get("nonce", 0)


def get_balance(rpc_url, address):
    r = requests.get(f"{rpc_url}/accounts/{address}/balance", timeout=10)
    data = r.json()
    return data.get("balance_sentri", 0)


def send_tx(rpc_url, tx, api_key=""):
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["X-API-Key"] = api_key
    body = {"transaction": tx}
    r = requests.post(f"{rpc_url}/transactions", json=body, headers=headers, timeout=10)
    return r.status_code, r.json()


def wait_for_confirmations(rpc_url, initial_height, expected_tx_count, timeout_sec=120):
    """Wait until all transactions are included in blocks."""
    start = time.time()
    confirmed = 0
    while time.time() - start < timeout_sec:
        info = get_chain_info(rpc_url)
        current_height = info.get("height", 0)
        mempool_size = info.get("mempool_size", 0)

        # Count blocks produced since start
        new_blocks = current_height - initial_height
        # Rough estimate: 100 tx/block max
        estimated_confirmed = min(new_blocks * 100, expected_tx_count)

        if mempool_size == 0 and new_blocks > 0:
            return {
                "confirmed": expected_tx_count,
                "blocks": new_blocks,
                "time_sec": time.time() - start,
                "final_height": current_height,
            }
        time.sleep(1)

    return {
        "confirmed": expected_tx_count - info.get("mempool_size", 0),
        "blocks": info.get("height", 0) - initial_height,
        "time_sec": time.time() - start,
        "final_height": info.get("height", 0),
        "timeout": True,
    }


def run_benchmark(rpc_url, signer, from_addr, tx_count, chain_id, api_key=""):
    """Run a single TPS benchmark with tx_count transactions."""
    print(f"\n{'='*60}")
    print(f"  Benchmark: {tx_count} transactions")
    print(f"  RPC: {rpc_url}")
    print(f"  From: {from_addr}")
    print(f"{'='*60}")

    # Generate a random target address (self-transfer is fine for benchmarks)
    # Use a burn address to avoid balance checks
    to_addr = "0x" + hashlib.sha256(os.urandom(32)).hexdigest()[:40]

    # Get starting state
    nonce = get_nonce(rpc_url, from_addr)
    balance = get_balance(rpc_url, from_addr)
    info = get_chain_info(rpc_url)
    initial_height = info.get("height", 0)

    print(f"  Starting nonce: {nonce}")
    print(f"  Balance: {balance / SENTRI_PER_SRX:.4f} SRX")
    print(f"  Chain height: {initial_height}")

    # Check balance is sufficient
    total_cost = tx_count * (MIN_FEE + 1)  # 1 sentri + min fee per tx
    if balance < total_cost:
        print(f"  ERROR: Insufficient balance. Need {total_cost / SENTRI_PER_SRX:.4f} SRX, have {balance / SENTRI_PER_SRX:.4f} SRX")
        return None

    # Pre-generate all transactions
    print(f"  Generating {tx_count} signed transactions...")
    gen_start = time.time()
    txs = []
    for i in range(tx_count):
        tx = create_signed_tx(
            signer, from_addr, to_addr,
            amount=1,  # 1 sentri per tx
            fee=MIN_FEE,
            nonce=nonce + i,
            chain_id=chain_id,
        )
        txs.append(tx)
    gen_time = time.time() - gen_start
    print(f"  Generated in {gen_time:.2f}s ({tx_count / gen_time:.0f} tx/s signing speed)")

    # Send all transactions as fast as possible
    print(f"  Sending {tx_count} transactions...")
    send_start = time.time()
    sent = 0
    failed = 0
    errors = {}
    for tx in txs:
        try:
            status, resp = send_tx(rpc_url, tx, api_key)
            if status == 200 and resp.get("success"):
                sent += 1
            else:
                failed += 1
                err = resp.get("error", "unknown")
                errors[err] = errors.get(err, 0) + 1
        except Exception as e:
            failed += 1
            err = str(e)[:50]
            errors[err] = errors.get(err, 0) + 1
    send_time = time.time() - send_start
    print(f"  Sent {sent}/{tx_count} in {send_time:.2f}s ({sent / send_time:.0f} tx/s submit rate)")
    if failed:
        print(f"  Failed: {failed}")
        for err, count in errors.items():
            print(f"    {err}: {count}")

    if sent == 0:
        print("  ERROR: No transactions accepted!")
        return None

    # Wait for confirmations
    print(f"  Waiting for confirmation...")
    result = wait_for_confirmations(rpc_url, initial_height, sent, timeout_sec=180)

    total_time = result["time_sec"]
    confirmed = result.get("confirmed", 0)
    blocks = result.get("blocks", 0)

    tps = confirmed / total_time if total_time > 0 else 0
    e2e_time = send_time + total_time

    print(f"\n  Results:")
    print(f"    Transactions sent:      {sent}")
    print(f"    Transactions confirmed: {confirmed}")
    print(f"    Blocks produced:        {blocks}")
    print(f"    Submit time:            {send_time:.2f}s")
    print(f"    Confirmation time:      {total_time:.2f}s")
    print(f"    End-to-end time:        {e2e_time:.2f}s")
    print(f"    TPS (confirmed/time):   {tps:.1f}")
    print(f"    TPS (e2e):              {confirmed / e2e_time:.1f}" if e2e_time > 0 else "    TPS (e2e):              N/A")
    if result.get("timeout"):
        print(f"    WARNING: Timed out waiting for confirmations")

    return {
        "tx_count": tx_count,
        "sent": sent,
        "confirmed": confirmed,
        "failed": failed,
        "blocks": blocks,
        "submit_time_sec": round(send_time, 2),
        "confirm_time_sec": round(total_time, 2),
        "e2e_time_sec": round(e2e_time, 2),
        "tps_confirmed": round(tps, 1),
        "tps_e2e": round(confirmed / e2e_time, 1) if e2e_time > 0 else 0,
        "errors": errors,
    }


def main():
    import argparse
    parser = argparse.ArgumentParser(description="Sentrix TPS Benchmark")
    parser.add_argument("--rpc", default=RPC_URL, help="RPC endpoint URL")
    parser.add_argument("--count", type=int, default=0, help="Single test with N transactions")
    parser.add_argument("--key", default=PRIVATE_KEY_HEX, help="Private key hex for test wallet")
    parser.add_argument("--api-key", default=API_KEY, help="API key for write endpoints")
    args = parser.parse_args()

    if not args.key:
        print("ERROR: Set SENTRIX_BENCH_KEY env var or pass --key <private_key_hex>")
        print("  This should be a testnet wallet with sufficient SRX balance.")
        sys.exit(1)

    signer = SimpleECDSA(args.key)
    from_addr = signer.address()
    print(f"Benchmark wallet: {from_addr}")

    # Verify connectivity
    try:
        info = get_chain_info(args.rpc)
        print(f"Connected to chain_id={info['chain_id']} height={info['height']}")
    except Exception as e:
        print(f"ERROR: Cannot connect to {args.rpc}: {e}")
        sys.exit(1)

    results = []

    if args.count > 0:
        r = run_benchmark(args.rpc, signer, from_addr, args.count, CHAIN_ID, args.api_key)
        if r:
            results.append(r)
    else:
        # Run standard 3-tier benchmark
        for count in [100, 500, 1000]:
            r = run_benchmark(args.rpc, signer, from_addr, count, CHAIN_ID, args.api_key)
            if r:
                results.append(r)
            else:
                print(f"\n  Skipping remaining tests (previous test failed)")
                break
            time.sleep(5)  # cool down between tests

    # Summary
    if results:
        print(f"\n{'='*60}")
        print("  TPS Benchmark Summary")
        print(f"{'='*60}")
        print(f"  {'Test':>10} {'Sent':>6} {'Confirmed':>10} {'Time':>8} {'TPS':>8}")
        print(f"  {'-'*10} {'-'*6} {'-'*10} {'-'*8} {'-'*8}")
        for r in results:
            print(f"  {r['tx_count']:>10} {r['sent']:>6} {r['confirmed']:>10} {r['e2e_time_sec']:>7.1f}s {r['tps_e2e']:>7.1f}")

        print(f"\n  Comparison:")
        print(f"    Ethereum:  ~15 TPS")
        print(f"    BSC:       ~100 TPS")
        print(f"    Solana:    ~3,000 TPS")
        avg_tps = sum(r["tps_e2e"] for r in results) / len(results)
        print(f"    Sentrix:   ~{avg_tps:.0f} TPS (this benchmark)")

    # Save results to JSON
    output_file = os.path.join(os.path.dirname(__file__), "results.json")
    with open(output_file, "w") as f:
        json.dump({"benchmarks": results, "timestamp": int(time.time())}, f, indent=2)
    print(f"\n  Results saved to {output_file}")


if __name__ == "__main__":
    main()
