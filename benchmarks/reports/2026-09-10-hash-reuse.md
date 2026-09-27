# Whole-blob hash reuse during chunk upload

The chunk writer now reuses its own completed BLAKE3 digest when FastCDC emits
a first chunk covering the entire input after EOF has been observed. This
removes one full chunk hash and one blocking-worker dispatch for that shape.
Previously, only inputs below the FastCDC minimum used a prehashed upload.
The independent repository check against dishonest backend identities remains.

`ChunkUploader` also collects the shared upload dependencies into one borrowed
context, removing the per-chunk handle clones and repeated argument lists.
Deduplication, compression, pins, byte-budget permits, upload concurrency, and
publication ordering retain their existing behavior. There is no additional
payload buffer, lookahead read, or thread pool.

## Boundaries and correctness

With the default settings, the original small-file path applies below 128 KiB.
The additional reuse applies to complete single chunks that FastCDC emits
after observing EOF while filling its 512 KiB buffer. A first chunk ending at
the buffer limit does not itself prove EOF, even if the file happens to end
there. Multi-chunk objects also retain their separate chunk hashes.

The permanent `hash_write_boundaries` group in `benches/write_path.rs` has 28
cases: random and zero-filled input at 0 B, 64 B, 1 KiB, 16 KiB, 64 KiB,
128 KiB minus/exact/plus one byte, 256 KiB, 512 KiB minus/exact/plus one byte,
1 MiB, and 4 MiB. It is registered in `benchmarks/manifest.json` under
`core-primitives` and included in `benchmark all` through `write_path`.

Each cold write checks its returned size; outside the timed region, each
iteration checks the whole-blob digest, chunk identities and sizes against
standalone FastCDC, and full readback. Four new reader tests additionally
exercise Pending, full read buffers, prefilled buffers, read failures, exact
input lengths without EOF, and chunker behavior across minimum/maximum bounds.

Validation passed: 178 active blob tests (11 ignored benchmark/helper tests),
the repository's dishonest-writer rejection test, 17 benchmark harness tests,
all-features/all-targets Clippy with warnings denied, formatting, and diff checks.
The default-feature test build emits an existing unused `CountingCompactState`
warning in `src/git_repository.rs`; the all-features Clippy check is clean.

## Measurements and limitation

[Raw measurements](2026-09-10-hash-reuse.json) retain all 80 measured cases,
Criterion estimates and raw samples, source and binary hashes, run order,
commands, and environment identity. The host was an AMD Ryzen 7 7840S Linux
workstation using Rust 1.96.0 and optimized builds.

One full before/after pair showed lower 128 KiB write times but higher times
in several 512 KiB cases. A longer reverse-order boundary recheck encountered
concurrent compiler jobs; even unchanged cases varied severalfold. The timing
comparison is **inconclusive** and must not be used as a speedup or regression
claim. All correctness gates passed in both pairs. The established improvement
is the removal of redundant hashing and worker dispatch for eligible inputs;
an isolated paired run is needed to quantify its effect on wall time.

These synthetic sizes are not a representative repository histogram. This
investigation does not establish thresholds for adding parallel hashing of
small-object batches or threading within a large whole-blob hash.

## Reproduction

The baseline production code is commit
`450a6336550e5faeb4d37c563a728ebfacaabbb3`, with the new benchmark added before
building it. Build that revision and the candidate with the same benchmark
source, toolchain, and features:

```sh
devenv shell cargo bench --features experimental --bench write_path --no-run
```

Preserve each executable printed by Cargo as `target/hash-reuse-before` and
`target/hash-reuse-after`. On an otherwise idle host, run:

```sh
CRITERION_HOME=target/hash-reuse-criterion target/hash-reuse-before --bench hash_write_boundaries --warm-up-time 1 --measurement-time 3 --save-baseline before
CRITERION_HOME=target/hash-reuse-criterion target/hash-reuse-after --bench hash_write_boundaries --warm-up-time 1 --measurement-time 3 --save-baseline after
```

Repeat in reversed order before drawing a performance conclusion. Exact
short-run and boundary-recheck commands used here are retained in the JSON.
