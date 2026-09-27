# Current tar baseline: quiet-host admission failed

The existing tar benchmark was rebuilt from main `598ef961aa36c3be4ce8c7dfdb43cc80b94ee660` with
the local profiling/order controls. All 16 release correctness cases passed:
canonical and published roots, entry/file counts, logical bytes, and full
payload readback. No production code was changed for this attempt.

The requested four timing repetitions did not start. The 180-second admission
wait failed: all 162 sampled intervals contained competing build processes.
Median sampled external CPU was 83.20% of logical CPU capacity, with a
maximum of 95.78%. The existing policy requires ten quiet seconds, no
recognized competing build/benchmark processes, and at most 5% external CPU.

No new throughput estimate or bottleneck conclusion is supported. No further
CPU profile was collected under this contention. The existing processes were
left running; CI and billing were untouched.

The [retained evidence](2026-09-11-tar-current.json) includes the build result,
commit/source/binary fingerprints, local harness patch, correctness log, exact
timing configuration, and every activity interval. Binary and source hashes
were checked again after the failed admission. Raw artifacts and the immutable
binary are in `benchmarks/results/2026-09-11-tar-current/`.

## Reproduce

Run on an idle Linux host, using a fresh output directory:

```sh
devenv shell cargo bench --features experimental --bench tar_import --no-run
/path/to/copied-tar_import-BUILD_ID --test
python3 -m benchmarks.tar_compare \
  --binary /path/to/copied-tar_import-BUILD_ID \
  --output /tmp/tar-current-new --repetitions 4 --quiet-timeout 180
```

Copy the executable reported by Cargo before measuring so later builds cannot
replace it. The permanent `tar-import-pipeline` cases already cover both sides
of the 16-file concurrency bound and remain registered in
`benchmarks/manifest.json` and `benchmark all`. This attempt adds no benchmarks.
