# Tar CPU profile: hashing and memory operations dominate large imports

Six scoped profiles completed with the permanent tar matrix's import and
full-readback correctness gates. Large and mixed archives spent roughly
34–36% of sampled user CPU in BLAKE3 and 28–30% in memory operations.
Compression symbols accounted for 15–17%, and FastCDC scanning for 7–8%.

| Archive | In-flight files | BLAKE3 | Memory operations | Zstd symbols | FastCDC | Recorded samples |
|---|---:|---:|---:|---:|---:|---:|
| 256 × 1 KiB | 1 | 10.23% | 18.73% | 7.91% | 0.00% | 3,202 |
| 256 × 1 KiB | 16 | 8.77% | 17.13% | 13.80% | 0.00% | 5,170 |
| 4 × 4 MiB | 1 | 35.53% | 29.17% | 14.81% | 7.67% | 2,215 |
| 4 × 4 MiB | 16 | 34.06% | 29.77% | 15.29% | 8.38% | 2,210 |
| 32 × 1 KiB + 4 × 4 MiB | 1 | 34.10% | 28.04% | 15.63% | 7.08% | 2,147 |
| 32 × 1 KiB + 4 × 4 MiB | 16 | 33.79% | 30.28% | 17.43% | 7.68% | 2,434 |

Memory operations group memcpy/memmove/memset/memcmp symbols. Other columns
use the existing profile runner's symbol categories. These are exclusive self
costs across the importer and worker threads, not wall-time phase durations.
Rounded symbol percentages and inlining limit the precision of attribution.
Fixtures contain deterministic random data and use in-memory storage.

## Concrete next experiment

Inspect bounded chunk decompression first. In the large-file/concurrency-16
profile, resolved memmove caller edges include the zstd streaming reader
(4.71% of sampled user CPU), zstd decompression internals (2.90%), and vector
growth through realloc (1.00%). Import-side copies also appear in
StreamReader (2.90%), FastCDC drain_bytes (2.44%), and the Tokio duplex stream.
The complete caller output is embedded in the JSON receipt. Some ancestor
addresses are unresolved, so these local edges do not establish full call
chains or the fraction of cost removable by any proposed change.

Source inspection found that
[`decompress_capped`](../../../src/blob/chunked/manifest.rs) constructs a new
streaming decoder and grows an initially empty Vec for each chunk.
The existing [`compression::decompress`](../../../src/compression.rs) helper
already reuses a bulk decoder for sized, single-frame input and retains a
bounded streaming fallback for unsized or concatenated frames. Chunk writes
use the corresponding bulk compressor.

A focused next experiment is to reuse that existing bounded helper for chunk
decoding and measure whether it reduces decoder buffering, growth, and setup.
Keep output limits, exact-size checks, digest verification, truncated-frame
rejection, and legacy-frame behavior covered by correctness tests. This is a
candidate for measurement, not a demonstrated optimization; production code
was not changed during this profiling task.

## Scope and background activity

Perf used cpu-clock:u at 499 Hz with inherited worker sampling and acknowledged
FIFO enable/disable commands. Events were enabled only around request.import,
including repository verification/publication. Fixture creation and benchmark
readback were excluded, though their correctness assertions still ran.
All recordings reported zero lost samples.

The profiling runner records load but does not apply the timing admission
guard. Median/peak external CPU percentages were:

| Case | Median | Peak |
|---|---:|---:|
| small-256 / 1 | 9.79% | 15.27% |
| small-256 / 16 | 47.41% | 68.05% |
| large / 1 | 49.65% | 56.07% |
| large / 16 | 45.99% | 52.29% |
| mixed / 1 | 45.17% | 56.38% |
| mixed / 16 | 51.24% | 55.87% |

Five profiles exceeded the user's 30% background timing ceiling. They are
retained as diagnostic CPU attribution and do not qualify as accepted timing
runs or demonstrate a throughput improvement. The earlier
[three accepted timing matrices](../2026-09-13-tar-cpu30-retry/README.md)
remain separate evidence. No other processes were paused, and CI/billing were
unchanged.

## Evidence and reproduction

The [receipt](report.json) embeds all six flat reports, benchmark/perf logs,
activity records, sample counts, the copy caller report, source/build/binary
provenance, and recording hashes. The executable remained immutable and
matched the recorded production and benchmark sources. Raw perf recordings
remain in benchmarks/results/2026-09-13-tar-profile/.

```sh
python3 -m benchmarks.tar_profile \
  --binary benchmarks/results/2026-09-12-tar-baseline/tar-baseline \
  --perf /path/to/perf --output /tmp/tar-profile-new --seconds 10
```

Create the copy caller report using the same immutable binary and recording:

```sh
/path/to/perf report --stdio --no-children --call-graph graph,0.5,caller \
  --symbol-filter __memmove_avx512_unaligned_erms \
  -i /tmp/tar-profile-new/large-16.perf.data \
  > /tmp/tar-profile-new/large-16.memmove-callers.txt
python3 benchmarks/reports/2026-09-13-tar-profile/summarize.py \
  /tmp/tar-profile-new /tmp/tar-profile-summary.json \
  benchmarks/results/2026-09-12-tar-baseline/provenance.json
```

Use the provenance of the selected binary when rebuilding. The six cases are
part of the existing tar matrix, registered in benchmarks/manifest.json and
included in benchmark all. No new benchmark cases were introduced.
