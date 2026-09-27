# Native macOS setup reuse

Run the permanent `fskit-setup-reuse` suite, registered in
`benchmarks/manifest.json` and included in `benchmark all`:

```sh
CASITA_BENCH_NATIVE_FSKIT_SETUP=1 python3 -m benchmarks.cli all \
  --suites fskit-setup-reuse --profile smoke --repetitions 3 \
  --timeout 1800 --output RESULTS
```

The `initial/` cohort ran on macOS 26.6.2, Apple M1, at Casita
`ca10387316babfc111c4897b3592e8b6bc89ebda` with Turso
`dca55133caa690f90dcdd58d3c4329fb0703659c`. It reused an installed FUSE-T
runtime. All three runs passed correctness gates; environment, commands, raw
samples, source identity and artifact checks are retained alongside the logs.

In the test profile, one million calls per run to public `ensure()` averaged
14.61–15.10 ns per call after initialization. Initial native `ensure()` calls
in the three processes took 8.02 ms, 0.269 ms and 0.271 ms. These measurements
exclude process launch, filesystem mounting and Nix evaluation.

Persisted receipt checks used controlled fixture runtimes, averaging
58.68–59.78 microseconds per call. The identical-settings replacement case
averaged 58.35–60.96 microseconds. Both assert no subprocess execution. They
measure receipt validation rather than fresh installation. No cold runtime
installation was exercised.

The final `merged/` cohort ran at Casita
`ea6aa37382283b361b3c9aa0f0ab1229115aff08`, after incorporating current main.
All nine scenario samples and the completion/artifact gates passed. Warm native
reuse averaged 43.28–55.12 ns/call; first native calls took 3.1605 ms, 0.593 ms
and 0.5525 ms. Receipt fixtures averaged 63.83–65.02 microseconds, and the
settings-replacement case 56.13–57.81 microseconds. These are separate cohorts
on a shared machine, not a controlled before/after regression comparison.

The native paged-overwrite crash regression passed in 162.81 seconds before
this benchmark. Linux all-feature tests and the RustFS rerun are retained:
683 tests initially passed, 34 could not launch RustFS, and 35 were ignored.
After correcting PATH, all 59 WAL3 tests passed. All-feature/all-target Clippy
also passed with the matched Rust 1.96 toolchain and PROTOC configured.
