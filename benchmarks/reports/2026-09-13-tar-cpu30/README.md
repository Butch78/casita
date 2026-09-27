# Tar timing with a 30% background CPU budget

The user accepted 30% background CPU for this investigation. The permanent
tar runner now accepts `--max-external-cpu-percent 30` and
`--allow-competing-builds`: named build/benchmark processes are retained in
the activity record, while admission and measurement use the selected CPU
ceiling. The default remains 5% with the process veto for other callers.
The percentage is external process CPU divided by all logical CPU capacity;
it excludes the benchmark's own process tree.

This attempt failed its 180-second admission window before launching the
benchmark. Median external CPU was 64.40%, peaking at 88.44% across
150 intervals; 149 exceeded 30%.
No new timing or correctness run was collected. A superseded admission using
30% with the old process veto was interrupted and retained separately.
No process was paused, and no production code, CI, or billing was changed.

The [receipt](report.json) retains both admission attempts, exact commands,
activity samples, binary/build provenance, the runner patch, and validation
output. The immutable executable still matches all recorded production and
benchmark sources. All 232 Python harness tests passed with two skipped;
tests cover finite percentage validation, both sides of the 30% boundary,
process-veto selection, and policy propagation into the runner and report.

## Reproduce

```sh
python3 -m benchmarks.tar_compare \
  --binary benchmarks/results/2026-09-12-tar-baseline/tar-baseline \
  --output /tmp/tar-cpu30-new --repetitions 4 --quiet-timeout 180 \
  --max-external-cpu-percent 30 --allow-competing-builds
```

Use a fresh output directory and an immutable release binary from the desired
revision. This uses the existing 16 tar cases registered in the manifest and
`benchmark all`; no new benchmark cases were introduced. Reports at different
CPU budgets retain their own acceptance policies and must not be silently
combined into one comparison.
