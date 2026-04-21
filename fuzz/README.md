# Fuzzing

The checked-in corpus is intentionally small and curated. Keep only hand-picked
seeds that exercise distinct packet shapes; let local fuzz runs grow temporary
corpora and re-minimize them before checking anything in. The checked-in seeds
are documented in `fuzz/corpus/README.md`.

Run the focused packet-boundary fuzzers with nightly Rust:

- `cargo +nightly fuzz run fuzz_received_packet --target host-tuple -- -dict=dictionary.txt`
- `cargo +nightly fuzz run fuzz_packet_reader --target host-tuple -- -dict=dictionary.txt`
- `cargo +nightly fuzz run fuzz_serializer --target host-tuple -- -dict=dictionary.txt`
- `cargo +nightly fuzz cmin fuzz_received_packet --target host-tuple`
- `cargo +nightly fuzz cmin fuzz_packet_reader --target host-tuple`
- `cargo +nightly fuzz cmin fuzz_serializer --target host-tuple`

The fuzz crate enables the main crate's `fuzzing` feature to expose a minimal,
fuzz-only API surface for serializer, parser, and `PacketReader` entry points.
