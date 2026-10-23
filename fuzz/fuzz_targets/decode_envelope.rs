#![no_main]
// cargo-fuzz target for deep campaigns (nightly + cargo-fuzz required):
//   cargo fuzz run decode_envelope -- -max_len=4096
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = acl_core::Envelope::decode(data);
});
