//! Fuzz target: Transaction::signing_payload() determinism + no-panic.
//!
//! The signing payload is the byte stream every validator signs to attest
//! a transaction. INVARIANT: same Transaction -> same payload, byte-stable
//! across runs + across machines. A divergence here = signature mismatch
//! across the cluster = block rejection. Worse: a panic during payload
//! construction crashes the validator under adversarial input.
//!
//! We construct Transaction via direct field-init (bypassing Transaction::new
//! which requires a real secp256k1 keypair) so the fuzzer can drive any
//! shape of transaction, including malformed signature/public_key strings
//! a peer could send.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sentrix_primitives::transaction::Transaction;

#[derive(Arbitrary, Debug)]
struct Input {
    txid: String,
    from_address: String,
    to_address: String,
    amount: u64,
    fee: u64,
    nonce: u64,
    data: String,
    timestamp: u64,
    chain_id: u64,
    signature: String,
    public_key: String,
}

fuzz_target!(|input: Input| {
    let tx = Transaction {
        txid: input.txid,
        from_address: input.from_address,
        to_address: input.to_address,
        amount: input.amount,
        fee: input.fee,
        nonce: input.nonce,
        data: input.data,
        timestamp: input.timestamp,
        chain_id: input.chain_id,
        signature: input.signature,
        public_key: input.public_key,
    };

    // Invariant 1: signing_payload must not panic on any field combo.
    let p1 = tx.signing_payload();
    let p2 = tx.signing_payload();

    // Invariant 2: deterministic (same struct -> same payload).
    assert_eq!(p1, p2, "signing_payload not deterministic");
});
