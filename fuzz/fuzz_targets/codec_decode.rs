//! Fuzz target: sentrix-codec::decode must return Err, not panic, on
//! arbitrary bytes for every type validators receive from peers.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentrix_codec::decode;
use sentrix_primitives::block::Block;
use sentrix_primitives::transaction::Transaction;
use sentrix_bft::messages::{Proposal, Prevote, Precommit, RoundStatus};

fuzz_target!(|data: &[u8]| {
    // Each line: decode must terminate with Err, not panic / not loop.
    // libFuzzer wraps with a sigaction handler so any abort surfaces.
    let _ = decode::<Block>(data);
    let _ = decode::<Transaction>(data);
    let _ = decode::<Proposal>(data);
    let _ = decode::<Prevote>(data);
    let _ = decode::<Precommit>(data);
    let _ = decode::<RoundStatus>(data);
});
