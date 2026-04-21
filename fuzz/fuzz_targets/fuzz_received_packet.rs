#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    minimq::fuzzing::parse_received_packet(data);
});
