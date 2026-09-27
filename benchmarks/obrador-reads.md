# Obrador retained-reader workload

`obrador-reads` runs Obrador's existing shared-DAG NAR fixture through
`ObradorStore::read_file`, using its existing retained-reader integration. It
compares process-owned snapshot protection with a diagnostic durable control
built from the same Casita source. Only the control's `owned_read_hold()` admission
changes; Obrador's API implementation remains unchanged.

```sh
devenv shell python3 -m benchmarks.cli run obrador-reads \
  --obrador-source /path/to/obrador --profile standard --repetitions 3 \
  --output /tmp/obrador-reads.json

devenv shell python3 -m benchmarks.cli all \
  --obrador-source /path/to/obrador --profile smoke --output /tmp/corpus
```

The smoke corpus uses 12 registered paths; standard uses 12 and 100. Both run
one and eight concurrent readers, with physical GC off and on. Each standard
worker reads 200 files per repetition. Override `--paths`, `--workers`, and
`--iterations` for larger runs. `benchmark all` registers this suite and records
an explicit skip when no Obrador source is supplied.

Each sample checks exact file contents, all registered paths, and shared-DAG
references before and after collection. GC-on cases require a completed GC pass
started during the read workload. All registered Nix paths remain roots, so this
measures collection of a live application store rather than reclamation of a
large garbage backlog. It does not measure sandboxed Nix build throughput.

Reports retain individual read and collection durations and nearest-rank p50,
p95, and p99 read latency. The adjacent `.work` directory retains build logs,
binaries and their hashes, and snapshots of both source trees, including tracked
working-tree changes and Cargo.lock. Original checkouts are not edited. Use a
new output/work directory for each run. Builds require Obrador's dependencies and
a Rust native-build environment; compilation can take several minutes.

To extend a completed smoke run without rebuilding, pass
`--reuse-build-report /tmp/obrador-reads.json` with a new `--output`, the standard
profile, and the desired repetitions. The runner verifies both binary hashes
and carries forward the original source provenance; retain the first run's
work directory alongside the new report.

For a shared Linux host, add `--require-quiet-host`. Before each case the runner
waits for ten seconds without detected compiler, Nix, or Casita benchmark/test
processes and with external CPU use at most 5% of total host capacity. During
the case it samples external processes once per second, excluding the runner
and its children. Kernel-worker CPU is recorded separately because filesystem
workers may be executing the benchmark's own I/O. The busiest external user
processes are also recorded. Contaminated attempts retain their timings and activity logs
but are excluded from accepted samples; the runner retries up to three times.
`--quiet-timeout` bounds each wait (default 900 seconds). This detects common
competing jobs and CPU activity; it does not provide hardware isolation or
guarantee the absence of short-lived jobs and external disk activity.

```sh
python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report /tmp/obrador-reads.json --profile standard \
  --paths 100 --workers 8 --repetitions 5 --require-quiet-host \
  --output /tmp/obrador-reads-repeat.json
```

For CPU profiles, `--perf /path/to/perf` enables samples only around the timed
read/GC phase, using acknowledged perf control FIFOs. Setup imports and final
verification run with events disabled. Profiles retain data files, symbol/call
graphs, and hashes, and reject probes that do not acknowledge the scoped protocol
or produce no samples. Profiling timings are diagnostic and should not be mixed
with the unprofiled latency comparison.

`--rebuild-probe` recompiles the current benchmark example in the retained copied
workspaces using their existing Cargo.lock (`--locked`). It preserves the old
binaries, saves the old probe source, and records new binary/probe hashes. Use the
same Rust/native build environment as the original build to reuse dependencies.

To benchmark a Casita change with the exact same Obrador source, use
`--rebuild-casita --casita-source /path/to/casita` with `--reuse-build-report`.
The runner snapshots the selected Casita checkout, backs up the previous copied
Casita directories, and rebuilds both ownership variants in the retained
workspaces. It preserves Obrador's implementation, its existing Cargo.lock,
and the old benchmark binaries. The result records the new Casita source hashes.

The [reader-inventory cache investigation](reports/2026-09-09-obrador-reader-cache.md)
records the correctness gates, source provenance and standard matrix results.

```sh
devenv shell python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report /tmp/obrador-reads.json --rebuild-probe \
  --profile standard --paths 12,100 --workers 8 \
  --perf /path/to/perf --require-quiet-host \
  --output /tmp/obrador-reads-profile.json
```
