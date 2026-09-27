# Bounded tar import pipeline

Tar imports now overlap copying the next archive body with closing, verifying,
and staging earlier files. The parser is sequential; owned payload writers
pass through a bounded channel to concurrent finalizers. One admission permit
covers copying, queueing, closing, and verification, so the default limit of
16 applies to the entire pipeline rather than independently to each stage.
The producer and consumer are co-polled in the import future, avoiding a
memory-budget deadlock when a current writer needs permits held by an earlier
finalizer. No whole-file buffer or detached pipeline task is added.

`TarImportLimits.max_in_flight_files` defaults to 16; 1 selects serial file
processing. CLI users can set `--tar-max-in-flight-files COUNT`; IPC callers
can set `options.limits.max_in_flight_files`. Zero and unsupported oversized
limits fail validation before opening a writer. Raw writes remain scoped to
the mutation's data pin. The root is published only after all file finalizers,
link resolution, and directory staging succeed.

## Correctness

Six controlled pipeline tests prove bounded overlap and out-of-order completion,
cancellation and backend-error cleanup, duplicate/truncated-input handling,
limit validation, tiny shared chunk-budget progress, forward hard-link chains,
and old-GNU sparse expansion and limits. Together with the existing archive
coverage, 39 tests passed. The CLI and IPC archive-import integration tests,
17 benchmark-harness tests, all-features/all-targets Clippy, formatting, and
the docs build/link validation also passed.

## Permanent benchmarks

`benches/tar_import.rs` is registered in `benchmarks/manifest.json` and included
in `benchmark all` through `core-primitives`. It compares limits 1 and 16 for
0, 1, 15, 16, 17, and 256 one-KiB files, four 4-MiB files, and a mixed archive
of 32 one-KiB files plus four 4-MiB files. Thus both sides of the default
admission window remain covered. Every iteration validates its independently
constructed canonical root, published root, file/entry counts, total logical
bytes, and full payload readback outside the timed import region.

All 16 cases passed in correctness-only mode and in the timed run. The payload
backend is the real chunked backend over an in-memory object store; metadata
is in-memory. These are synthetic fixtures, not representative repository
histograms or remote-storage benchmarks.

## Exploratory measurements

The [raw report](2026-09-10-tar-pipeline.json) retains all estimates, samples,
configuration, commands, environment, and executable/source hashes. Medians
from one short local run, in milliseconds:

| Fixture | Limit 1 | Limit 16 |
|---|---:|---:|
| small-0 | 0.095 | 0.055 |
| small-1 | 0.118 | 0.119 |
| small-15 | 0.956 | 0.583 |
| small-16 | 1.001 | 0.612 |
| small-17 | 0.989 | 0.607 |
| small-256 | 14.377 | 8.161 |
| large | 36.692 | 30.054 |
| mixed | 37.670 | 31.837 |

The workstation was shared, compiler processes were observed at the end of
the run, and even the empty-archive control varied materially. These numbers
must not be used as a causal speedup claim or a regression bound. The controlled
concurrency tests establish that file finalizers actually overlap and stay
bounded; isolated repeated runs are needed to quantify throughput gains.

## Reproduction

```sh
devenv shell cargo test --features experimental --lib tar::
devenv shell cargo test --features experimental --bench tar_import
devenv shell cargo bench --features experimental --bench tar_import
```

The exact short-run options are in the JSON. Use an otherwise idle host and
repeat comparisons before making performance claims.
