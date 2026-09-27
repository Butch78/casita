# Obrador retained-reader latency — 2026-09-09

All 16 standard cases passed: 14,400 exact-content file reads, registered-path and shared-DAG reference checks before and after collection, and completed collection passes in every GC-on case. The full Casita default-feature and all-features test suites passed before this standard run, including retained-reader crash, snapshot replacement, and reader-lifetime tests.

This uses Obrador’s existing NAR fixture and `ObradorStore::read_file` integration. The durable control changes only Casita’s retained-reader admission in a copied source tree. Original Obrador API code was not edited.

| Paths | Readers | GC | Durable p95 (ms) | Process p95 (ms) | Process / durable |
|---:|---:|:---:|---:|---:|---:|
| 12 | 1 | off | 46.815 | 0.823 | 0.02× |
| 12 | 1 | on | 22.065 | 7.432 | 0.34× |
| 12 | 8 | off | 94.105 | 8.341 | 0.09× |
| 12 | 8 | on | 27.897 | 14.561 | 0.52× |
| 100 | 1 | off | 31.469 | 3.011 | 0.10× |
| 100 | 1 | on | 55.533 | 5.263 | 0.09× |
| 100 | 8 | off | 24.610 | 37.364 | 1.52× |
| 100 | 8 | on | 28.579 | 69.388 | 2.43× |

Process protection improved observed p95 in six configurations, but regressed in the 100-path/eight-reader cases, both with and without GC. The corpus retains both the smaller and larger store sizes. This single repetition ran on a shared workstation with other activity, so it does not establish a causal threshold or a universal performance improvement. Repeating the larger fanout cases on an idle host is needed before attributing the regressions to pin ownership.

Each worker performed 200 timed reads. Percentiles use nearest rank. The workload keeps registered paths rooted and runs physical collection against a live store; it does not measure a garbage backlog or sandboxed build throughput.

## Reproduction

See [the permanent suite documentation](../obrador-reads.md). The recorded run used:

```sh
devenv shell python3 -m benchmarks.cli run obrador-reads \
  --obrador-source /home/domen/dev/obrador --profile smoke \
  --output /tmp/casita-obrador-reads-smoke.json

python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report /tmp/casita-obrador-reads-smoke.json \
  --profile standard --output /tmp/casita-obrador-reads-standard.json
```

The [raw report](2026-09-09-obrador-retained-reads.json) retains every read duration, collection duration, source-file hashes, working-tree patch hashes, binary hashes, build environment, and measurement environment. Both builds use the recorded tracked working-tree contents, not just the listed base commits. The original source snapshots and build logs are retained in `/tmp/casita-obrador-reads-smoke.work`; preserve that directory to reproduce the exact dirty source state.

Casita source base: `6a42e0933494a24e14abf5fc65975e8b711b904a`. Obrador source base: `88f546af44717074d0a8a4105968c39f2b2ff311`. The runner is registered in `benchmarks/manifest.json` and included in `benchmark all` with `--obrador-source`.
