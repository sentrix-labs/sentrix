//! Fuzz target: BftMessage encode + decode roundtrip via sentrix-codec.
//!
//! Validators exchange BFT messages (Proposal / Prevote / Precommit /
//! RoundStatus) over libp2p. INVARIANT: decode(encode(msg)) == msg for
//! every constructible BftMessage. A roundtrip break = byte-stable wire
//! representation broken = fork class. A panic in decode = DoS attack
//! vector via a single crafted peer message.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentrix_codec::{decode, encode};
use sentrix_bft::messages::BftMessage;

fuzz_target!(|data: &[u8]| {
    // Phase A: decode random bytes. Must return Err, not panic.
    let _ = decode::<BftMessage>(data);

    // Phase B: if random bytes happen to parse as a valid message,
    // re-encode and confirm the encoding matches. (Some encodings have
    // canonical forms; roundtrip stability is the invariant.)
    if let Ok(msg) = decode::<BftMessage>(data) {
        let reencoded = match encode(&msg) {
            Ok(b) => b,
            Err(_) => return,
        };
        let msg2: BftMessage = match decode(&reencoded) {
            Ok(m) => m,
            Err(_) => panic!("re-encoded message failed to decode: {:?}", msg),
        };
        // Compare via re-encoding to avoid PartialEq requirement on every
        // variant. If both round-trips converge to the same bytes, the
        // serialization is canonical for this message.
        let re_reencoded = encode(&msg2).expect("encode(msg2) must succeed since msg encoded");
        assert_eq!(reencoded, re_reencoded, "non-canonical encoding for {:?}", msg);
    }
});
