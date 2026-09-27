# Tar baseline retry: complete matrix, rejected timing

The existing immutable tar benchmark completed all 16 cases, with the
canonical-root, published-root, counts, logical-bytes, and full payload
readback assertions active on every iteration. The binary and all recorded
source hashes still match the previous build from `598ef961aa36c3be4ce8c7dfdb43cc80b94ee660`.
No rebuild or production change was needed.

The guard admitted the run after 257.6 seconds. Another Clippy build
started during measurement, so the matrix was marked `contended` and the
remaining three repetitions were skipped. Of 75 measured activity
intervals, 52 observed recognized
competing build processes. Median external CPU was 8.95% of logical
CPU capacity, peaking at 29.60%; the policy permits at most 5% and no
recognized competing builds. Build jobs were observed in
`/home/domen/dev/casita5` during this attempt.

The retained case estimates are diagnostic data, not an accepted baseline.
No speedup, regression, or new bottleneck conclusion is supported by this run.
A reserved idle interval is still needed for all four repetitions. CI and
billing were unchanged, and other processes were left running.

[Raw evidence](2026-09-12-tar-current.json) retains all estimates and samples,
the exact command, admission/measurement activity, executable/source
fingerprints, and benchmark log. Original Criterion files remain in
`benchmarks/results/2026-09-12-tar-current/`.

## Reproduce

Use a fresh output directory and the immutable executable:

```sh
python3 -m benchmarks.tar_compare \
  --binary benchmarks/results/2026-09-11-tar-current/tar-current \
  --output /tmp/tar-quiet-new --repetitions 4 --quiet-timeout 600
```

To rebuild elsewhere, use `cargo bench --features experimental --bench tar_import --no-run` and copy the executable reported by Cargo before running
the same command with its path. These are the existing permanent
`tar-import-pipeline` cases registered in `benchmarks/manifest.json` and
included in `benchmark all`; no new benchmark cases were added.
