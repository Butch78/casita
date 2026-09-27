# Controlled local pack pipeline benchmark

`git-pack-delayed` compares serial reads, the production batch of eight, and the
experimental pipeline of eight using the production pack writer. It runs in
`benchmark all`. No sockets, S3, compressed-payload cache, utilization sampling,
or CPU admission gates are involved. This is a storage simulation, not a model
of S3 throughput or real network latency.

A memory backend has 1, 2, or 8 permits. Each payload read acquires a permit,
waits 0 or 5 ms, and retains that permit until the returned reader is dropped.
A memory sink accepts at most 64 KiB per write and waits 0 or 2 ms before each
accepted write. Tokio timer granularity affects these nominal delays. Both
profiles retain the full case matrix; use repetitions to control run length.

The small corpus contains 24 distinct pseudorandom 64 KiB blobs. The boundary
corpus contains 16 such blobs plus 1 MiB minus one byte, exactly 1 MiB, and
1 MiB plus one byte. Xorshift seed 9 makes bodies reproducible. Blob tags let
the benchmark select every object without tree traversal or delta encoding.
The fixture, service binding, baseline generation, and validation are untimed.

Each of the 24 configurations warms up all three modes, then rotates the mode
order across three rounds and reverses across the next three. Six rounds give
each mode two appearances in each position. The timer covers selection cloning,
ordering, payload reads, encoding, and output writes; byte equality, independent
native pack decompression, Git blob identity checks, SHA-1 trailer validation,
and statistics run afterward. A 30-second deadline detects stalled calls. All
reads must complete, active reader counts must stay within the configured bound,
and every permit must be returned. Every pack must match the production baseline.

Build the release library test executable and obtain its `executable` path from
the `compiler-artifact` Cargo JSON record with `profile.test=true`:

```sh
cargo --offline test --release --all-features --lib --no-run --message-format=json
python3 -m benchmarks.cli run git-pack-delayed \
  --probe-binary /path/to/frozen/casita-library-test \
  --repetitions 6 --output /tmp/delayed-pack.json
```

Use fresh output paths; the report retains raw measurements, separate warmups,
paired comparisons, binary and source fingerprints, and the complete helper log.
Only the named ignored benchmark executes. Existing focused Git-fetch unit tests
cover cancellation and resource release while output remains indefinitely stalled.
Production defaults are unchanged.
