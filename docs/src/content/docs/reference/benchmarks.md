---
title: Benchmarks
description: Run and interpret Casita's reproducible performance suites.
---

The [benchmark harness](https://github.com/cachix/casita/tree/main/benchmarks)
measures import, checkout, sync, verification, and collection. It also has
focused suites for repository scale, Git history, catalog indexing, cache
pressure, and network conditions. The [benchmark dashboard](/benchmarks/)
shows published results.

Comparisons with Git, restic, Borg, and tar+zstd cover only operations the tools
share. Their verification, retention, and storage semantics differ, so read the
notes beside each result before comparing timings.

## Reproduce a run

From the pinned development environment, start with a short smoke run:

```console
$ devenv shell
$ benchmark --profile smoke \
    --implementations casita,git,tar-zstd \
    --cache-policies warm \
    --repetitions 1
```

Each timed sample gets a fresh repository. Setup and correctness checks happen
outside the timing window. The runner checks restored file bytes, paths,
executable bits, and symlink targets before accepting a sample.

For publication, use the [full command and workload matrix in the benchmark
README](https://github.com/cachix/casita/blob/main/benchmarks/README.md#publication-run).
It uses both cache policies, all comparators, at least ten repetitions,
`--require-all`, and `--require-clean`. Raw JSON records the source revision,
tool versions, environment, inputs, and individual samples. Keep that JSON
with its generated Markdown report in `benchmarks/baselines/`.

To regenerate a report without rerunning a benchmark, use
`benchmark --render-existing RESULT.json`. Exploratory output belongs in
`benchmarks/results/`; `benchmark-dashboard` builds the public page from the
selected results.

## Graph traversal and spill

`graph-traversal` measures verification and collection when traversal state
spills to temporary SQLite databases:

```console
$ benchmark run graph-traversal --profile smoke --repetitions 1 \
    --output benchmarks/results/graph-traversal-smoke.json
```

The run records time, peak memory, spill size, and cleanup. Its correctness gate
requires a forced spill and checks that temporary files are removed. See the
[suite details](https://github.com/cachix/casita/blob/main/benchmarks/README.md#graph-traversal-and-spill)
for the tested sizes and limits.

## Native Git scale

`git-scale` measures import and Git service operations across histories with
many objects, deltas, large packs, and wide trees:

```console
$ benchmark run git-scale --profile smoke --shape many-objects
```

The optional huge profiles require a dedicated volume. Synthetic cases isolate
individual costs; a real large repository provides a separate check. See the
[Git scale suite](https://github.com/cachix/casita/blob/main/benchmarks/README.md#native-git-scale-suite).

## Catalog scale and S3 requests

`benchmark run catalog-index` measures catalog encoding, lookup, publication,
reopening, and rebasing. It records memory, transferred bytes, and object-store
requests. Its scale projections include request costs as well as catalog size.
For current thresholds and assumptions, use the
[catalog suite documentation](https://github.com/cachix/casita/blob/main/benchmarks/README.md#persistent-pack-index-benchmark).

## Retained history, cache pressure, and network limits

These suites test different sources of slowdown:

| Suite | What it varies |
|---|---|
| `history-scale` | Retained generations and small updates |
| `pack-cache-scale` | Working set size and read pattern against a bounded cache |
| `network-scale` | Latency and per-connection bandwidth for S3 and atomic RPC |

Run a bounded check with `benchmark run SUITE --profile smoke --repetitions 1`.
The [suite documentation](https://github.com/cachix/casita/blob/main/benchmarks/README.md#history-cache-and-network-scale-matrix)
explains the workloads, correctness checks, and limits of local network and
cache measurements.
