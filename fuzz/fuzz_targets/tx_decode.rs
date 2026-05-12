//! Fuzz target: TokenOp::decode + StakingOp::decode on adversarial
//! transaction `data` strings. Mempool admit calls these; a panic here
//! is a DoS vector (single bad tx crashes validator on next mempool
//! pull). Invariant: must return None (not panic, not infinite-loop)
//! on malformed input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentrix_primitives::transaction::{TokenOp, StakingOp};

fuzz_target!(|data: &[u8]| {
    // Bytes -> String (lossy) so we feed the actual decode API which
    // takes &str. Real tx data is base64-ish hex JSON; the fuzzer will
    // hit a lot of UTF-8 garbage which the decode path must tolerate.
    let s = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };

    let _ = TokenOp::decode(s);
    let _ = StakingOp::decode(s);
    let _ = TokenOp::is_token_op(s);
    let _ = StakingOp::is_staking_op(s);
});
