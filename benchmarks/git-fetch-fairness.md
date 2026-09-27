# Git fetch fairness

`git_fetch_fairness` exercises `GitFetchService::build_pack` with an in-memory
repository and a lightweight tag pointing to one blob. There is no cached native
pack. This deliberately exposes ready storage reads and ready output writes;
it is not an HTTP or disk throughput benchmark.

The 32 cases cross payload sizes 1,048,575, 1,048,576, 1,048,577, and 4,194,304
bytes, repeated-byte and deterministic random contents, one and four concurrent
fetches, and current-thread and four-worker Tokio runtimes. The first three sizes
cover both sides of the production 1 MiB buffered-encoding boundary.

Each case warms up, then emits three JSON diagnostic samples with completion
time, longest fetch-future poll, and longest scheduling gap of an unrelated
continuously ready task. Criterion measures completion with that observer
disabled. Poll timing remains enabled. Fixture creation and validation are
outside the measured interval. Each output must have a valid native pack header,
single blob entry, declared length, SHA1 trailer, complete zlib stream, and exactly
the input bytes. Validation runs on every iteration, including correctness-only
runs. Scheduling measurements are diagnostics, with no noisy timing assertions.

```sh
devenv shell cargo bench --features experimental,git-fetch --bench git_fetch_fairness -- --test
devenv shell cargo bench --features experimental,git-fetch --bench git_fetch_fairness
```

The benchmark is registered in `manifest.json` under `core-primitives` and is
included in `benchmark all`. The all-suite runner retains its JSON diagnostics
in `git_fetch_fairness.log` and collects Criterion estimates with the other core
benchmarks. Run timing studies on a quiet host and retain the environment and
raw samples; a scheduling difference here alone does not establish application
P99 impact. Large entries now preserve codec state across bounded blocking jobs:
at most 1 MiB of input and 1 MiB + 65,515 bytes of compressed output per job, with
eight active streaming jobs shared across clones of a fetch service. A batch
combines up to sixteen reads, each capped at 64 KiB and the configured read size.
Output is written in at most 65,515-byte chunks, yielding after each group of
four chunks. The last data job also finishes the codec. Reads and output
backpressure remain async; a canceled running job retains its admission permit
until it finishes. The existing size matrix remains the regression corpus for
the 1 MiB dispatch and batching boundary.
