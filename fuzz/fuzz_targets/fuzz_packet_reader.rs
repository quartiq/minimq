#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&storage_len, stream)) = data.split_first() else {
        return;
    };
    let mut storage = vec![0u8; usize::from(storage_len).max(1)];
    minimq::fuzzing::drive_packet_reader(&mut storage, stream, stream);
});
