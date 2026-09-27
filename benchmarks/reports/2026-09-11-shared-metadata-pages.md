# Shared metadata pages: fresh overwrite scaling

The permanent `overwrite_pages` target passed all 20 correctness cases and a
Criterion quick run. Each iteration uploads a new 300-byte replacement. Full
rehashing and readback verify the new content identity outside the timed edit.

The largest case is 256 MiB (16,384 explicit 16 KiB chunks). Its boundary-crossing
edit read 111,240 metadata bytes, wrote 55,648 metadata bytes, and peaked at
284,237 extra Rust heap bytes. Payload reads were 65,572 compressed bytes, covering
only the two affected chunks for proof generation and editing. The equivalent
flat chunk map and Bao outboard serialize to 1,703,880 bytes; that figure is a
format-size calculation, not a measurement of the previous implementation.

| Chunks | Edit | Time estimate (µs) | Metadata read (KiB) | Metadata write (KiB) | Extra peak Rust heap (KiB) |
|---:|---|---:|---:|---:|---:|
| 64 | within | 280 | 16.0 | 13.1 | 199.8 |
| 64 | across | 462 | 16.3 | 13.1 | 263.7 |
| 66 | within | 283 | 13.7 | 6.9 | 193.2 |
| 66 | across | 479 | 13.7 | 6.9 | 241.2 |
| 4096 | within | 876 | 31.4 | 15.7 | 199.6 |
| 4096 | across | 805 | 76.7 | 38.4 | 266.9 |
| 4097 | within | 533 | 31.6 | 15.8 | 199.6 |
| 4097 | across | 841 | 76.9 | 38.5 | 267.1 |
| 4098 | within | 495 | 31.8 | 15.9 | 199.7 |
| 4098 | across | 692 | 31.8 | 15.9 | 247.6 |
| 16384 | within | 610 | 37.2 | 18.6 | 203.3 |
| 16384 | across | 1061 | 108.6 | 54.3 | 277.6 |

The difference between 4,097 and 4,098 chunks in the boundary-crossing case
comes from the selected middle offset and the affected Bao paths, as well as
tree geometry. Both sides remain in the permanent corpus. Extra metadata work
tracks the affected pages and their paths; the format contains no edit-history
chains. Small original flat maps pay a bounded conversion cost during an edit.

The test matrix covers 63/64/65/66 and 4,095/4,096/4,097/4,098/4,099 chunks,
straddling flat-storage and fanout thresholds for both metadata trees. It also
includes 16,384 chunks. Every case checks full BLAKE3 identity, full readback,
and limits on payload reads, metadata reads/writes, and peak extra Rust heap.

## Reproduce

```console
cargo bench --features experimental --bench overwrite_pages -- --test
cargo bench --features experimental --bench overwrite_pages -- --quick
cargo bench --features experimental --bench overwrite_pages
```

Registered in `benchmarks/manifest.json` and in `benchmark all` through
`core-primitives`. See the [benchmark instructions](../README.md#fresh-edits-with-shared-metadata-pages)
for the fixture, allocation accounting, gates, and timing exclusions.

Environment: AMD Ryzen 7 7840S, Linux 7.2.2, rustc 1.97.1, in-memory object
store, current-thread Tokio runtime, deterministic seed 91. Source was the
working tree based on `d69a9c4`. This quick run overlapped compilation and tests;
latency intervals are noisy and are not an isolated performance comparison.
The heap counter covers Rust allocations, excluding native codec workspace and
allocator internals. Reported timings also include allocator-accounting overhead. Root publication,
disk, and network latency are outside this benchmark.

[All 20 cases and timing intervals](2026-09-11-shared-metadata-pages.json).
