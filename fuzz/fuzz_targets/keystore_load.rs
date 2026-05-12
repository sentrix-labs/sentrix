//! Fuzz target: Keystore JSON deserialization. A bug here on a malicious
//! keystore file = potential wallet-load vulnerability. Invariant: must
//! return Err, not panic, on any byte stream.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sentrix_wallet::keystore::Keystore;

fuzz_target!(|data: &[u8]| {
    // The Keystore is a serde-derived struct loaded from disk via
    // serde_json::from_str. We fuzz from_slice directly to skip the
    // filesystem layer (cargo-fuzz can't write the path).
    let _ = serde_json::from_slice::<Keystore>(data);
});
