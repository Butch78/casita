# S3-backed Git HTTP fetch

This permanent suite exercises the production S3/wal3 repository, Git pack
generation, HTTP server, and a real Git client. It starts a private RustFS
instance on loopback and deletes only its own temporary storage on completion.
An explicit `--fixture-dir` retains its private storage for reuse instead.
It requires the tools in `devenv.nix` and an explicitly built helper:

```sh
cargo build --release --features s3,git-http,experimental --example git_fetch_s3
benchmark run git-fetch-s3 --probe-binary /path/to/release/examples/git_fetch_s3 \
  --repetitions 2 --output benchmarks/results/git-fetch-s3.json
```

Add `--nixpkgs /path/to/nixpkgs` to test that clone's committed HEAD tree. The
runner makes a parentless benchmark commit referencing the exact tree, without
modifying the source repository or importing its history or working-tree edits.
The report retains both original revision and tree IDs. Keep the source clone
available throughout the run. A source checkout at the recorded revision
reproduces the corpus.

For comparisons, add `--baseline-binary /path/to/before/git_fetch_s3`. Both
executables must use the same helper and dependencies, with separate build
directories to prevent stale artifact reuse. Upload happens once using the
candidate, then both versions read the same immutable S3 repository.
Two repetitions run before/after and after/before. Binaries are fingerprinted
before and after execution. There is no CPU utilization gate or quiet-host wait.

The default corpus has text and deterministic random blobs of 1 MiB minus one
byte, exactly 1 MiB, 1 MiB plus one byte, 4 MiB, and 16 MiB. This covers both
sides of the encoder handoff threshold and heavier chunk-upload contention.
`benchmark all` runs this corpus and also runs
the nixpkgs corpus when given `--nixpkgs`.

Each server uses four Tokio workers and serves one fetch at a time. Each sample
uses an empty Git client. Server startup, S3 open, view binding, client setup,
and post-fetch checks are outside the timer. Timed Git fetch includes HTTP,
server traversal, S3 payload reads, compression, client indexing and its
`fetch.fsckObjects` validation. Post-fetch gates check exact commit and tree,
reachable object count, and `git fsck --full --strict`. Per-server payload
counters must show S3 reads. Logs and individual sample times are retained.

Native cached Git packs are disabled to exercise pack generation. Payload
caches are tested at zero, 16, 64, and 128 MiB (override with `--cache-mib`).
These capacities bracket the boundary fixture and full nixpkgs payload footprints.
Each process serves a first fetch and three checked repeat fetches by default
(`--repeat-fetches` overrides this count), using its remaining cache state;
these repeats are not a guarantee
that the entire corpus fits in cache. OS and RustFS caches are not flushed.
Counters cover all fetches together by default. Results measure loopback S3 behavior,
not AWS latency, full-history clones, or concurrent-client throughput.

Fixture setup imports Git locally, then bulk-transfers the verified native view
and its full closure into S3. Local staging is discarded before fetch timing.
The initial nixpkgs investigation hit `pin ledger remained contended` with
direct S3 native import at default concurrency; a serial retry spent roughly
ten minutes in setup before cancellation. `--fixture-import direct` retains
that serial setup path for reproduction. Neither setup path changes fetch limits.

For retries, `--prepared-local-nixpkgs /path/to/casita-repository` can reuse a
previously imported local Casita view named `snapshot`. It requires `--nixpkgs`
and local-transfer setup. The helper checks the prepared view's exact expected
commit and rejects cached native packs before upload; all client correctness
gates still apply. The supplied local repository is retained.

The investigation also exercises remote pin coordination: handles on one
storage client share their edit queue, and concurrent protections on one lease
are merged. When comparing encoders, include those same pin fixes in both
executables and retain their source hashes. This isolates compression from
changes to S3 admission and staging.

Preferred follow-up: repeat on the deployment's actual S3 endpoint and network.
Other options are concurrent clients or additional payload-cache capacities.

## Diagnosing repeat-fetch slowdowns

`--diagnostics` records counter deltas separately for each fetch, including cache
evictions, and wall time in `git.fetch.write_pack` and
`git.fetch.streaming_entry`. The latter covers only objects larger than 1 MiB;
it includes storage waits, codec work, and output backpressure, not CPU time.
Instrumentation is filtered to those two span names. Every diagnostic sample
requires one pack span and the exact expected number of streaming-entry spans.
Both comparison binaries must contain the same helper and tracing annotations.

For cached-path investigations, use `--cache-mib 128 --repeat-fetches 8` with
the nixpkgs fixture, checking zero payload requests in every repeat. Every
repeat uses a fresh empty Git client and the same correctness gates. Samples
retain `fetch_index` (zero for the first fetch) and the server's `repetition`.
Compare `git.fetch.write_pack` time separately from total fetch time, and
report medians per server as well as overall: multiple repeats in one server
are correlated, not independent process repetitions. The default three repeats
also run through the manifest entries and `benchmark all`.

The same matrix covers bounded small-object read-ahead: use the current
streaming encoder with serial payload reads as the baseline, and change only
the small-object read window in the candidate. The boundary corpus covers
objects below, at, and above the 1 MiB read-ahead cutoff. Include nixpkgs to
measure many small objects, with 64 and 128 MiB caches to cover repeat-fetch
misses and a fully cached repeat. Keep both capacities even if only cache
misses improve; cache-hit overhead is part of the comparison. Read-ahead keeps
up to eight ordered payload futures per fetch, separately from the existing
bounded encoder queue and the backend's request and byte limits.
Reads complete in batches of up to eight before waiting on output, so a slow
client cannot leave read futures holding backend request permits. Batches stop
at every large object before streaming it.

`benchmark run git-fetch-local` is a separately registered local-storage control
using the same helper, corpus, Git clients, cache matrix, and correctness gates.
It is included in `benchmark all`. With `--prepared-local-nixpkgs`, it serves the
retained local repository directly, checking its exact commit and absence of a
cached native pack before binding. It does not start RustFS or measure S3.

```sh
benchmark run git-fetch-local --probe-binary /path/to/after \
  --baseline-binary /path/to/before --diagnostics \
  --nixpkgs /path/to/nixpkgs --prepared-local-nixpkgs /path/to/local-casita \
  --cache-mib 0 64 128 --repetitions 2 --output local-control.json
benchmark run git-fetch-s3 --probe-binary /path/to/after \
  --baseline-binary /path/to/before --diagnostics \
  --repetitions 4 --output s3-boundary-diagnostics.json
```

An A/A control passes the same immutable binary to both binary options. This
measures variation between labels without an encoder source difference; it must
not be described as a before/after speedup.

For expensive full-tree S3 setup, use `--fixture-dir /path/to/private-fixture`.
The runner holds a nonblocking exclusive filesystem lock, records a fixture
identity only after import succeeds, and refuses to reuse a different corpus.
Every server still verifies the expected native commit and every client still
runs all integrity checks. Interrupted imports have no completed marker and are
retried. The path is retained on both success and failure; each run's logs remain
in its separate report artifacts. Only use a directory dedicated to this suite.

`--fixture-import direct --import-concurrency 128` uses native concurrent import
for untimed setup. The default direct concurrency remains one for reproducing
earlier serial probes; the native source-byte budget stays at 64 MiB. This option
does not affect local-transfer setup or fetch concurrency. The report records
the actual setup mode/concurrency and whether its fixture was reused.

```sh
benchmark run git-fetch-s3 --probe-binary /path/to/after \
  --baseline-binary /path/to/before --diagnostics --nixpkgs /path/to/nixpkgs \
  --fixture-import direct --import-concurrency 128 \
  --fixture-dir /path/to/private-fixture --cache-mib 64 128 \
  --repetitions 2 --output s3-nixpkgs-diagnostics.json
```
