# Ordered metadata pagination — 2026-09-08

Ordered inventory pagination now seeks within the current namespace and advances to later namespaces when that range is exhausted. Both continuation queries use bounded searches on the compound key index. This removes the repeated sorting identified in the [earlier investigation](2026-09-08-ordered-scan.md), while preserving canonical order and 256-record pages.

At 65,537 objects, the median warm scan fell from 14.028 s to 220.104 ms: 63.7× faster, a 98.43% reduction. At 8,192 objects it fell from 164.559 ms to 15.738 ms. These are matched local measurements of the public ordered inventory API.

## Paired measurements

Three alternating baseline/candidate pairs, five sizes per variant, one first scan and one warm scan per fresh process: 30 processes and 60 timed scans. Every process passed exact ordered-record and snapshot-revision checks. Values below are medians of three observations, in milliseconds.

| Objects | Baseline first ms | Fixed first ms | Baseline warm ms | Fixed warm ms |
|---:|---:|---:|---:|---:|
| 256 | 0.860 | 0.965 | 0.435 | 0.575 |
| 257 | 0.845 | 0.927 | 0.469 | 0.457 |
| 8,192 | 166.246 | 19.701 | 164.559 | 15.738 |
| 65,536 | 13846.343 | 247.790 | 13872.864 | 212.797 |
| 65,537 | 14110.753 | 233.692 | 14028.310 | 220.104 |

The 256-object warm median increased by 0.140 ms (32.2%); the first median increased by 0.106 ms. Exhausting an inventory now requires the additional namespace-advance query. Small cases also show substantial run-to-run variation, so the data does not establish a precise overhead estimate. The 257-object warm median was essentially unchanged. The shared host was not isolated, and three pairs do not provide formal confidence intervals.

The registered `metadata-scan` fixture seeds verified blobs in batches of 1,024 and reopens the store before scanning. Timing includes decoding and collecting the full stream into a vector. Setup, reopen, equality/revision audits, and query plans are outside timing. First does not imply cold OS cache. Process RSS includes fixture and audit allocations. The timed fixture contains one namespace; mixed namespaces are covered by the correctness regression below. These measurements do not establish end-to-end GC or fsck performance.

## Query plans and correctness

The old OR continuation reports `MULTI-INDEX OR` followed by `USE SORTER FOR ORDER BY`. The implemented queries report:

```text
namespace-range:
SEARCH objects USING INDEX sqlite_autoindex_objects_1 (namespace=? AND native_id>?)

namespace-after:
SEARCH objects USING INDEX sqlite_autoindex_objects_1 (namespace>?)
```

The permanent probe now retains both implemented plans alongside the old OR and tuple diagnostics. All raw process outputs are embedded in the receipt.

The new regression covers an empty inventory; namespace sizes 255, 257, 1, 512, and 1; empty, NUL-containing, prefix, and non-UTF-8 native IDs; and reversed, negative, gapped physical row IDs. It pauses a stream across a page boundary, removes objects and inserts a new object through concurrent commits, then verifies that the old snapshot still yields the exact original records. New and reopened snapshots must yield the exact committed inventory and revision.

Validation: all 474 enabled library tests passed in one run (16 ignored), including the process-crash tests. The targeted SQLite run passed 20 tests (one ignored). All-feature/all-target Clippy with warnings denied, formatting, and diff whitespace checks passed. All six benchmark results also passed dashboard normalization, with first and warm observations kept separate.

## Build identity and reproduction

The baseline is `dcec23496c23c22ca2d9663aa744ff0471b98385`; the candidate is that revision plus the source patch embedded in the receipt. Both release probes use Rust 1.96.0, `--features cli`, identical locked dependencies, optimization level 3, and the package-only override `profile.release.package.casita.codegen-units=256`.

Default release builds were terminated by host earlyoom under memory pressure. Increasing codegen units allowed both probes to compile. The candidate used one build job and the baseline two; this changes build scheduling, not optimization settings. The earlier investigation used a different codegen profile, so its timings are not used as the baseline here. A shared-cache reuse was caught by checking binary hashes before measurement; the baseline package was cleaned and freshly rebuilt. The receipt records the two distinct retained binary hashes and successful build logs.

In separate baseline and candidate checkouts, restore the receipt's Cargo.lock and build each probe with the same override:

```console
cargo --config 'profile.release.package.casita.codegen-units=256' test -j 1 --release --locked --features cli --lib --no-run --message-format=json
```

Retain the library test executable identified by Cargo's compiler-artifact output from each build. Alternate these commands for three pairs, using separate output paths:

```console
benchmark run metadata-scan --profile standard --counts 256,257,8192,65536,65537 --iterations 1 --repetitions 1 --no-build --probe-binary /absolute/path/to/retained-probe --output benchmarks/results/metadata-scan-variant-pair.json
```

The suite remains registered in `benchmarks/manifest.json`, `benchmark all`, revision comparisons, and dashboard normalization. Both sides of the 256-row page boundary and 65,536-object scale boundary remain in the permanent corpus. The [machine-readable receipt](2026-09-08-ordered-paging.json) includes all six runs, raw samples and plans, source patch, source files, dependency lock, binary identities, successful build logs, validation logs, and the comparison orchestrator.
