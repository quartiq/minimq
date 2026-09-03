# Fuzzing

The checked-in corpus is intentionally small and curated. Keep only hand-picked
seeds that exercise distinct packet shapes; let local fuzz runs grow temporary
corpora and re-minimize them before checking anything in. The checked-in seeds
are documented in `fuzz/corpus/README.md`.

Run the focused packet-boundary fuzzers with nightly Rust:

- `cargo +nightly fuzz run fuzz_received_packet -- -dict=dictionary.txt`
- `cargo +nightly fuzz run fuzz_packet_reader -- -dict=dictionary.txt`
- `cargo +nightly fuzz run fuzz_serializer -- -dict=dictionary.txt`
- `cargo +nightly fuzz cmin fuzz_received_packet`
- `cargo +nightly fuzz cmin fuzz_packet_reader`
- `cargo +nightly fuzz cmin fuzz_serializer`

The fuzz crate enables the main crate's `fuzzing` feature to expose a minimal,
fuzz-only API surface for serializer, parser, and `PacketReader` entry points.
