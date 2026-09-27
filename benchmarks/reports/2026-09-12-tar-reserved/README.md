# Tar timing with paused builds

All 16 existing tar cases completed with their canonical/published-root,
counts, bytes, and full payload-readback assertions active. Timing was
rejected: 33 of 77 activity intervals exceeded 5% external CPU.
Median external CPU was 4.74% of logical CPU capacity and the maximum
was 17.01%, primarily Chromium in the largest burst. New build processes
also ran briefly before the controller paused them; the guard retained those
intervals as contention. The remaining three repetitions were skipped.

With explicit user authorization, the controller paused four processes from
Cargo build trees in Casita and Obrador workspaces, then sent SIGCONT to all
four in its cleanup. A subsequent PID/start-time/state check confirmed that
none remained stopped. No browser, agent, system service, or unrelated user
process was paused. CI and billing were unchanged.

The [receipt](report.json) retains binary/build provenance, all raw timing
samples and estimates, host activity, the pause/resume ledger, verification of
resume, and exact controller/guard identities. The original files are under
`benchmarks/results/2026-09-12-tar-reserved/`. These measurements do not
establish a baseline, speedup, or new production bottleneck. No profile was
collected after the rejected baseline.

## Stopped-process accounting

The existing guard previously vetoed stopped compilers as active competitors.
It now records them separately only if both endpoint states are stopped,
PID/start time match, and CPU ticks are unchanged. Sleeping builds, newly
stopped processes, resumed processes, running children, and any measured CPU
still count normally. The 5% threshold and ten-second admission interval are
unchanged. Regression tests cover those distinctions and the controller's
PID-reuse and watchdog resumption behavior.

## Reproduce

The optional [controller](run.py) requires explicit permission to pause other
local builds. It monitors known build processes owned by the current user,
pauses their descendants, runs the unchanged four-matrix tar comparison,
and resumes processes on completion or failure. An independent watchdog
resumes them if the controller exits or after a maximum of fifteen minutes.
It does not reserve desktop CPU or prevent a new build from running briefly
before its next scan, so the usual timing guard remains necessary.

```sh
python3 benchmarks/reports/2026-09-12-tar-reserved/run.py \
  benchmarks/results/2026-09-12-tar-baseline/tar-baseline \
  /tmp/tar-reserved-new
```

Use an immutable executable built from the desired revision and a fresh output
directory. The existing `tar-import-pipeline` cases remain registered in
`benchmarks/manifest.json` and included in `benchmark all`; no new benchmark
cases were introduced. Reliable follow-up timing needs an idle dedicated host
or a window with automated launches and desktop CPU bursts stopped at source.
