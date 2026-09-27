# Tar baseline after benchmark publication: contended

A freshly compiled immutable release executable from `f0b369b6fca2ef0e3bb3c38f2c3a833f5e0b9f26` completed
all 16 existing tar cases. Every timed iteration retained independent
canonical/published-root, entry/file-count, byte-count, and full-readback
correctness checks. The build used an unchanged tracked checkout, and binary
and source fingerprints were checked again after measurement.

The first matrix was admitted after 517.0 seconds. It was rejected as
contended: 16 of 77 measured intervals exceeded the 5% external
CPU limit, and 14 intervals detected a `cc1` compiler. Median external CPU
was 3.92% of logical CPU capacity, with a peak of 10.99%. Chromium
was the largest external CPU consumer in the highest-load intervals; an
Obrador job also appeared in the activity samples.

The user agreed to hold off starting other local work during admission.
Previously observed jobs drained, but additional background activity still
overlapped the benchmark. No other process was stopped or reconfigured.

The remaining three repetitions were skipped by the existing runner. These
estimates are retained as diagnostic evidence, not an accepted baseline; no
new profile or production optimization is justified by this timing attempt.
Production code, CI, and billing were unchanged.

[Retained evidence](2026-09-12-tar-baseline.json) includes the exact command,
source/build/binary provenance, all admission and measurement activity,
Criterion estimates and samples, and the benchmark log. The executable and
original Criterion files remain in
`benchmarks/results/2026-09-12-tar-baseline/`.

## Reproduce

Build the registered `tar_import` target and copy the executable reported by
Cargo before measuring:

```sh
cargo bench --features experimental --bench tar_import --no-run
python3 -m benchmarks.tar_compare \
  --binary /path/to/copied-tar-import \
  --output /tmp/tar-baseline-new --repetitions 4 --quiet-timeout 600
```

The same 16 permanent cases are registered in `benchmarks/manifest.json` and
included in `benchmark all`. This attempt introduces no new benchmark cases.
A follow-up needs an uninterrupted window without automated builds or large
background CPU bursts as well as without manually launched jobs.
