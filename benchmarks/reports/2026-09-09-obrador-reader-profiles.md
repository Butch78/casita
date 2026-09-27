# Scoped CPU profiles for retained Obrador reads

The [five-repetition latency run](2026-09-09-obrador-retained-reads-quiet.md) repeated a GC-off p95 regression at 100 paths and eight readers. These profiles investigate that result without changing Obrador’s API implementation or Casita’s production code.

All eight profiling cases passed exact-content, registered-path, reference-graph and host-activity gates. Events were enabled with an acknowledged perf control command after warm-up and disabled after the timed read/GC phase. Fixture imports and final verification were excluded. The probe was rebuilt with this control protocol in the retained copied workspaces using their existing Cargo.lock; the binaries from the latency comparison were preserved.

| Paths | GC | Durable user CPU (s) | Process user CPU (s) | Durable BLAKE3 self samples | Process BLAKE3 self samples |
|---:|:---:|---:|---:|---:|---:|
| 12 | off | 0.265 | 0.655 | 3.04% | 21.42% |
| 12 | on | 0.325 | 0.639 | 6.17% | 18.81% |
| 100 | off | 0.455 | 1.491 | 16.73% | 43.94% |
| 100 | on | 1.299 | 2.016 | 8.49% | 35.80% |

At 100 paths with GC off, the process variant used about 3.3× the sampled user CPU. BLAKE3 accounts for roughly 0.58 seconds of the approximately 1.04 seconds of additional sampled CPU. The process profile also contains reader-inventory encode/decode frames and associated memory copying. This points to whole-inventory serialization and checksumming as the primary optimization hypothesis.

The measured source’s `register_cached_reader()` updates the reader inventory and calls `write_readers()`. That path encodes all owner inventories, appends an outer BLAKE3 checksum, and replaces the sidecar under the admission lock. `ReaderState::decode()` validates the checksums on read. The durable control benefits from the existing incremental journal path. The next targeted experiment should reduce whole-inventory work while retaining the same admission fence and process-owner lifetime semantics.

These are sampled CPU observations, not proof of the p95 cause. User-CPU sampling excludes blocked lock/I/O time. Optimized and assembly frames sometimes unwind incompletely, so the table uses symbol self samples; it does not attribute all BLAKE3 work to a particular caller. Percentages are summed from rounded perf output. Profiling timings must not be mixed with the unprofiled latency results.

## Reproduction

```sh
devenv shell python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report benchmarks/reports/2026-09-09-obrador-retained-reads.json \
  --rebuild-probe --profile standard --paths 12,100 --workers 8 \
  --perf /path/to/perf --require-quiet-host \
  --output /tmp/casita-obrador-reads-scoped-profile-fixed.json
```

The [raw report](2026-09-09-obrador-reader-profiles.json) retains source/probe/binary hashes, individual read durations, host activity, CPU summaries, and paths to perf data and call graphs. Flat symbol reports are committed below. Original perf data and call graphs remain in `/tmp/casita-obrador-reads-scoped-profile-fixed.work`. The [permanent suite](../obrador-reads.md) covers both 12 and 100 paths and remains registered in the corpus.

- [12 paths, durable, GC off](2026-09-09-obrador-reader-profiles/0-12-8-False-durable-0.perf-flat.txt)
- [12 paths, process, GC off](2026-09-09-obrador-reader-profiles/0-12-8-False-process-0.perf-flat.txt)
- [12 paths, process, GC on](2026-09-09-obrador-reader-profiles/0-12-8-True-process-0.perf-flat.txt)
- [12 paths, durable, GC on](2026-09-09-obrador-reader-profiles/0-12-8-True-durable-0.perf-flat.txt)
- [100 paths, durable, GC off](2026-09-09-obrador-reader-profiles/0-100-8-False-durable-0.perf-flat.txt)
- [100 paths, process, GC off](2026-09-09-obrador-reader-profiles/0-100-8-False-process-0.perf-flat.txt)
- [100 paths, process, GC on](2026-09-09-obrador-reader-profiles/0-100-8-True-process-0.perf-flat.txt)
- [100 paths, durable, GC on](2026-09-09-obrador-reader-profiles/0-100-8-True-durable-0.perf-flat.txt)
