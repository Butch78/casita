# Direct cached Git pack generation

`git-pack-cached` measures production pack generation into an in-process `Vec`.
It opens a local Casita repository and uses its 128 MiB compressed-payload cache.
There is no S3 endpoint, HTTP server, socket transport, or Git fetch client.
Every measured call must make zero backend payload requests. Native Git pack
caching is disabled: cached chunks still need decoding and Git pack encoding.

```sh
cargo --offline build --release --features git,git-fetch,experimental \
  --example git_pack_cached
benchmark run git-pack-cached --probe-binary target/release/examples/git_pack_cached \
  --repetitions 10 --output benchmarks/results/git-pack-cached.json
```

Add `--nixpkgs /path/to/nixpkgs` for the committed tree of a local checkout.
The driver makes the same deterministic parentless snapshot as the Git-fetch
suites; it does not fetch history or change the source repository. To reuse
previously imported local data, also provide
`--prepared-local-nixpkgs /path/to/casita-repository`. Its `snapshot` view must
match the expected commit and contain no native cached Git pack. Otherwise
the driver imports the snapshot locally outside timing.

The permanent manifest entry and `benchmark all` include serial reads and
batches of 2, 4, and 8, plus the experimental `pipeline-8` mode. They always cover text and random blobs below, at, and
above 1 MiB, plus 4 and 16 MiB streaming objects. `benchmark all --nixpkgs ...`
also includes the full tree. The serial path reads and submits each object
immediately, without the intermediate batch allocation. All modes share the
same encoder implementation, output order, streaming barriers, runtime with
four workers, and encoder admission limit. Production still defaults to eight
reads; the hidden experimental benchmark hook permits only bounds 1 through 8.

One pipelined warmup fills the cache and exercises local payload misses.
Every round measures all five modes. The order rotates for five rounds and
reverses for the next five, balancing positions over ten rounds.
Preparation, repository binding, warmup, pack-file writes,
hash checks, Git indexing and correctness checks are outside the reported
generation timer. The timer covers `benchmark_build_pack`, including request
selection and in-memory pack output. There is no CPU utilization sampling or
admission gate.

Every output is indexed into a fresh local bare Git repository with
`git index-pack --stdin --strict`, checked against the expected commit/tree
and reachable object count, and validated with `git fsck --full --strict`.
Pack-byte digests must also match across all modes, warmup and diagnostics.
The driver waits for these gates before generating the next pack. Reusable
local input is retained; temporary imports, pack files and validators are
removed after the run. Reports and helper logs are retained, including failures.

Detailed tracing runs separately after primary measurements, once per mode:

- `read` spans cover opening, decoding/verifying and collecting each buffered
  payload. They do not isolate decompression alone.
- `encode` spans start inside the blocking worker and cover encoding one
  buffered object. Streaming objects retain their separate production path.
- `handoff` measures the interval between a payload-read span closing and its
  encoder span starting. It includes batch completion waits, intervening work,
  and blocking-pool queueing; it is not pure queue time.

Diagnostic counts must equal the number of buffered objects, and each encode
must match an already completed read by catalog index. Diagnostic spans have
overhead and overlap across objects, so their duration sums are not components
that can be added to reconstruct wall time. Diagnostic generation times are
excluded from the primary medians. Normal measurements keep these trace spans
disabled; the inexpensive disabled-span checks remain in the shared code.

`pipeline-8` submits each payload to the encoder as soon as its ordered read
completes. While an encoded entry waits on the worker or output sink, the
pipeline keeps polling the remaining reads in that finite window, storing
completed payloads in a bounded queue. No next window is admitted until all
eight payloads have been submitted. Output stalls therefore cannot leave
read futures suspended while holding backend permits. Cancellation drops
pending reads; large objects remain window boundaries. The existing encoder
queue bound is unchanged, and this mode does not change the production default.

Use the per-round primary times to judge a batch-size effect. Use separate
handoff distributions to investigate its mechanism, not to claim a precise
critical-path attribution. The preferred follow-up is to repeat any apparent
gain with the same local workload before changing the production batch size;
alternatively, investigate allocations if handoff time does not explain it.
