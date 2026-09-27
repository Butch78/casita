# Multi-root filesystem import spike

Retain `experimental::MultiRootFilesystemImport`. It materially reduces discovery and publication costs, with no median total-time regression in the measured cases.

## Main comparison

Five fresh-process repetitions per case, shuffled in a reproducible order. Each file contains 4 KiB of deterministic, distinct data. Both modes use exactly one mutation session. Times below are medians; ranges are minimum to maximum total time.

| Outputs | Files/root | Separate imports ms (range) | Multi-root ms (range) | Total reduction | Discovery reduction | Publication reduction |
|---:|---:|---:|---:|---:|---:|---:|
| 2 | 1 | 129.44 (62.23–144.81) | 84.37 (37.40–99.72) | 34.8% | 51.9% | 45.2% |
| 8 | 1 | 434.07 (231.22–466.59) | 114.93 (58.39–119.40) | 73.5% | 62.1% | 85.0% |
| 32 | 1 | 1969.28 (1799.20–2862.88) | 130.42 (125.34–255.62) | 93.4% | 73.7% | 98.1% |
| 2 | 28 | 146.05 (77.06–256.93) | 100.48 (52.68–106.27) | 31.2% | 44.2% | 45.8% |
| 8 | 28 | 546.26 (330.91–613.70) | 233.10 (113.15–304.45) | 57.3% | 48.5% | 82.6% |
| 32 | 28 | 2291.68 (1312.49–3497.95) | 614.52 (333.81–624.84) | 73.2% | 61.6% | 93.1% |

For the eight-output, one-file case, discovery falls from 1.687 to 0.640 ms and publication from 303.701 to 45.542 ms. Publication dominates this fixture. The optimization shares page dispatch, file-ingestion concurrency, bounded checkpoints, and final maintenance; it still inventories every filesystem entry. A traversal driver is not a count of physical directories visited.

Each mode opens every supplied path as a separate directory handle. No parent tree is synthesized and no input files are copied for the import. Roots are tagged by input index through traversal, ingestion and directory construction. Named roots are published together only after all trees have been constructed. Root names must be unique, and traversal limits apply to the complete request.

## Boundary comparison

Three repetitions per mode at 32 outputs. All six boundary cases improved in median total time. Persistent-storage timings vary substantially between processes, so these are local medians rather than portable speedup guarantees.

| Files/root | Separate ms | Multi-root ms | Total reduction | Discovery reduction | Publication reduction | Multi-root pages | Multi-root publications |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 27 | 3417.99 | 298.45 | 91.3% | 70.0% | 96.6% | 1 | 1 |
| 29 | 3216.38 | 398.90 | 87.6% | 68.0% | 96.0% | 2 | 1 |
| 30 | 1261.03 | 391.55 | 69.0% | 64.5% | 88.2% | 2 | 1 |
| 123 | 2227.65 | 1515.29 | 32.0% | 36.0% | 82.3% | 4 | 1 |
| 125 | 2258.55 | 1276.50 | 43.5% | 40.4% | 84.5% | 5 | 2 |
| 126 | 2340.15 | 1482.76 | 36.6% | 38.4% | 85.0% | 5 | 2 |

The Unix fixture has `files + 4` traversal entries and `files + 3` staged objects per output. At 32 outputs, 28 files exactly fill one 1,024-entry page; 29 files require two pages. At 125 files, 4,096 staged objects trigger a bounded checkpoint followed by final root publication. At 126 files, the remainder is published with the roots. Separate imports use 32 pages and 32 publications for every boundary case here.

## Decision and next step

Keep the experimental API. Discovery and publication both fall materially in the main matrix; all measured page and publication boundary cases also improve overall. Single-root requests retain the separate-walk strategy, and the benchmark keeps it as a same-binary control.

Next, wire one request containing all output `(path, root)` pairs into Obrador and repeat the existing eight-output end-to-end test. The 32-output workload is a useful follow-up scaling check. The Casita spike does not establish an end-to-end build-time percentage.

## Measurement limits

Linux 7.2.2, AMD Ryzen 7 7840S, performance governor, Rust 1.96.0. Sources and persistent repositories are on local Btrfs, with sources warmed by fixture creation. The baseline is the existing separate-walk/publication strategy inside the same instrumented binary, not a historical checkout. The worktree is based on `7682b01b9bebe776f3994be25d2699ffc65f346a`.

Discovery includes waiting for blocking-pool filesystem discovery. It is included in traversal/staging time, so those columns must not be added together. Independent phase medians need not sum to the total median. RSS covers the complete process, including fixture construction and audits. These measurements cover synthetic Casita imports; end-to-end Obrador impact and other storage/platform behavior still need measuring.

Every timed sample validates independently constructed tree digests, exact root mappings, complete payload bytes, and a clean fsck outside timing. Nested directories, empty directories, executable bits and escaping symlink text are included. Exact traversal-page and publication counts are asserted. No build or test job from this investigation ran concurrently with the main or boundary timing runs.

## Reproduction

Run these commands in the pinned `devenv shell`. A prebuilt release library test binary may be supplied with `--probe-binary PATH --no-build`.

```console
benchmark run filesystem-outputs --files 1,28 --repetitions 5 --output main.json --report main.md
benchmark run filesystem-outputs --counts 32 --files 27,29,30,123,125,126 --repetitions 3 --output boundaries.json --report boundaries.md
benchmark all --suites filesystem-outputs --repetitions 1 --output /tmp/filesystem-outputs
```

The permanent standard matrix additionally runs all boundary sizes at 2 and 8 outputs. The smoke matrix runs 2, 8 and 32 outputs at 1, 27, 28, 29 and 30 files. Its retained `all-smoke.json` and `all-execution.json` verify all-suite integration. That smoke run overlapped validation and is excluded from the performance comparison.

## Validation

- Rust library suite with `cli,experimental`: 736 passed, 43 opt-in probes ignored.
- Public importer integration suite: 10 passed, including the multi-root API.
- All-feature/all-target Casita Clippy with warnings denied passed.
- All-feature Casita documentation with warnings denied passed.
- Python benchmark tests: 344 passed.
- Formatting and whitespace checks passed.

Failure coverage forces a cross-root traversal-limit failure after durable checkpoints, verifies all prior root names remain unchanged, checks that remaining findings are collectible, and verifies clean fsck after collection. Other tests cover repeated relative names across pages, cache reuse, serial file concurrency, empty input, duplicate names and missing input directories.
