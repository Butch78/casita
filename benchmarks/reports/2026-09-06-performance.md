# Performance improvements — 2026-09-06

Three changes were retained: reuse an unchanged packed catalog, reduce pack
over-fetching under cache pressure, and pipeline RPC metadata/payload commands.
The OID SQL batching experiment was discarded after alternating measurements
did not establish a reliable benefit.

## Measured results

Values are medians across three process repetitions. Publication uses the
median of each process's per-update p50; idle sessions use the median of each
process's three observations. These are local measurements, not performance
guarantees. [The summary data](2026-09-06-performance.json) includes all measured
scales, request counts, latency percentiles, and binary fingerprints.

| Workload | Before | After | Result |
| --- | ---: | ---: | --- |
| Tiny publication, 100 retained roots | 6.12 ms | 2.96 ms | 52% lower latency |
| Tiny publication, 1,000 retained roots | 9.84 ms | 3.89 ms | 60% lower latency |
| Tiny publication, 10,000 retained roots | 176.36 ms | 27.46 ms | 6.4× faster |
| Idle mutation start, 10,000 retained roots | 184.91 ms | 1.20 ms | Catalog reconstruction avoided |
| Shuffled scan, 32 MiB working set / 8 MiB cache | 1,122.83 MiB fetched | 510.33 MiB fetched | 55% fewer backend bytes |
| Skewed reads, same cache pressure | 199.55 MiB fetched | 82.39 MiB fetched | 59% fewer backend bytes |
| Sequential scan, same cache pressure | 640.58 MiB fetched | 640.58 MiB fetched | Traffic unchanged |
| RPC, warm source, 100 ms RTT, unlimited rate | 518.22 ms | 315.70 ms | 39% lower latency |
| RPC, cold source, same network | 521.29 ms | 341.36 ms | 35% lower latency |
| Direct S3, cold source, same network | 1,409.60 ms | 1,420.74 ms | Request shape preserved |

The publication workload retains 100, 1,000, and 10,000 roots. Setup uses
64-root batches, followed by 100 individual 256-byte updates at each point.
This does **not** represent 10,000 consecutive single-pack publications.
Each cache phase reads 512 MiB logically, through 8,192 reads of 64 KiB objects.
The shuffled workload repeats one deterministic permutation; it is not
independent sampling with replacement.

## What changed

**Catalog refresh.** Read and hash the authoritative pointer on every refresh,
then retain the installed index and local overlay when the pointer is unchanged.
Independent writers remain visible. CAS retries still reconstruct authoritative
state, and a witness becomes reusable only after its index was installed
successfully. At 10,000 roots, an idle refresh reads 160,478 catalog bytes instead
of 2,897,680: the pointer GET remains, while the immutable-run GET and decoding
disappear. Delta application removes affected pack locations in one pass instead
of repeatedly scanning the growing chunk index. Duplicate locations, tombstones,
and exact pack reactivation retain their semantics.

**Pack cache.** Whole packs and exact ranges share the existing byte budget.
Before eviction pressure, distinct misses retain eager whole-pack promotion.
After pressure, scattered misses retain ranges and consecutive misses can promote
their pack. Admission history is bounded to 4,096 packs and cleared on pressure.
Promotions replace cached ranges rather than charging for both, and a concurrent
eviction no longer makes a reader discard successfully fetched bytes.

There is a request/byte tradeoff: the shuffled scan changes from 7,200 backend
requests to 8,164 (**13% more**), while skewed reads change from 1,309 to 1,318.
The cache improvement is a byte-amplification result; it is not a demonstrated
latency win for every remote cache-pressure workload. A combined cache-pressure
and high-RTT benchmark remains useful. The fitting 4 MiB working set still serves
all warmed reads without backend requests.

The first cache prototype required consecutive misses immediately. That regressed
the fitting S3 workload to 70 range requests. It was replaced: the retained policy
uses the original one range request plus one whole-pack request there.

**RPC.** Send existing `OBJECTS` and `PAYLOADS` commands before awaiting their
responses. Read while writing so small duplex buffers cannot deadlock. Both
existing batch capabilities must be negotiated; no wire-format change is needed.
The path fixture still sends five commands, but they occupy three dependent
exchanges. Pipelining is limited to 64 missing native blob/directory keys under a
1 MiB payload reservation, leaving room for a read buffer. Existing destination
records or payloads, smaller budgets, unsupported peers, and oversized payload
batches retain the ordinary paths. Payload identity, namespace, revision, and
path-proof verification remain in the receiver. Cancellation or malformed
pipelined responses poison the connection.

**OID experiment.** Combining four namespace queries into one SQL execution did
not reliably improve the 16,384-object workload with its unchanged 8,192-entry
cache. Alternating baseline/candidate runs gave local header medians of
43.28/44.08 µs, warm finds of 51.19/47.78 µs, and cold finds of 48.71/49.56 µs.
The implementation was removed. No larger cache default or OID speedup is claimed.

## Method and coverage

The baseline is `b81e6465e5af88c60a48108cc6221ad84ca1558e`; the retained release
binaries were built from optimization commit `58dd384` before rebasing onto
`6907796`. That upstream refactor changes API visibility/imports in these
benchmark paths, not their timed loops. Dependency lockfiles are identical.
The headline history results were then repeated in alternating order using the
final rebased binary, including a single-pack removal shortcut that avoids hash
probes when a delta affects only one pack. Its source identity and binary hash
are recorded separately in the summary JSON.

History runs alternated before/after order. The final comparisons ran without
overlapping this task's builds or tests. Earlier runs affected by build/test
contention are retained as diagnostics and excluded from the headline table.
The machine was a Ryzen 7 7840S, 16 logical CPUs, approximately 27 GiB RAM,
Linux 7.0.10, Btrfs, and Rust 1.96.0. It was a shared workstation; filesystem
reopen timings and small latency differences remain sensitive to host load.
Reopen at 10,000 roots improved from 301.94 to 117.39 ms; at 1,000 roots it
changed from 14.12 to 14.99 ms, so no universal reopen improvement is claimed.

The network matrix covers direct S3 and RPC, RTTs of 0/25/100 ms, and rates of
unlimited/1,024/8,192 KiB/s, with three repetitions and cold/warm source phases:
108 samples per version. Each phase receives into a fresh in-memory destination.
Rates apply per connection in each direction, not to an aggregate shared link.
“Cold” means a fresh Casita cache, not a flushed operating-system page cache.
The RPC relay models the network, not OpenSSH encryption overhead.

Correctness gates cover all retained roots, exact bytes, reopen, and clean fsck
for history; every timed cache read is compared byte-for-byte outside its timer;
network phases verify the selected closure and installed destination root.
There are 324 retained comparison samples and 114 samples for the discarded OID
experiment, in addition to the earlier diagnostic runs.

Validation includes the all-feature, default-feature, and portable Rust suites,
Clippy with warnings denied, rustdoc with warnings denied, formatting, and
135 Python tests. The publication crash matrix passed all 181 process kills and
fresh-process audits. Intermediate runs encountered a RustFS startup 503 and an
existing immediate file-lock-release assertion during parallel tests; their logs
are retained alongside subsequent verification.

The API rebase also required benchmark helpers to enable `experimental`.
Revision comparisons now read each checkout's target feature declarations so
older revisions are not passed a feature they do not have.

## Reproduction and receipts

Use `python3 -m benchmarks.cli run history-scale`, `pack-cache-scale`,
`network-scale`, and `gix-odb --profile cache-pressure`. Supply separately built
release binaries with `--probe-binary`, `--helper`, or `--benchmark-bin`, plus
`--no-build`. History used `--idle-sessions 3`; cache used `--reads 8192`;
all comparisons use three repetitions per version.

Raw results, build logs, test logs, source patches, binaries, and run ledgers are
retained locally under `benchmarks/results/2026-09-05-improve/` (gitignored).
The primary files are `paired-final-history-{before,latest}-{1,2,3}.json`,
`{before,retained}-cache-clean.json`, `{before,retained}-network.json`,
`paired-oid-{before,final}-{1,2,3}.json`, and `checks.json`.
The committed summary JSON preserves the numerical receipts and binary hashes.
