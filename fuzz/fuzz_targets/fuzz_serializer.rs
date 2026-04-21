#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&buf_len, rest)) = data.split_first() else {
        return;
    };
    let tag = rest.first().copied().unwrap_or(0);
    let qos = rest.get(1).copied().unwrap_or(0);
    let retain = rest.get(2).copied().unwrap_or(0) & 1 != 0;
    let aux = rest.get(3).copied().unwrap_or(0);
    let body = rest.get(4..).unwrap_or(&[]);
    let split = body
        .first()
        .map(|marker| usize::from(*marker) % body.len().max(1))
        .unwrap_or(0);
    let (topic_bytes, payload) = body.split_at(split.min(body.len()));
    let topic = String::from_utf8_lossy(&topic_bytes[..topic_bytes.len().min(32)]);
    let mut buf = vec![0u8; usize::from(buf_len)];

    minimq::fuzzing::encode_packet(
        &mut buf,
        tag,
        topic.as_ref(),
        &payload[..payload.len().min(128)],
        qos,
        retain,
        aux,
    );
});
