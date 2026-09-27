# Retirement comparison with a 15% external CPU budget

The user explicitly accepted a 15% external CPU allowance on September 11.
This supersedes the 5% limit for this comparison. Saved matched binaries from
`450a633` and `b279897` and their shared lockfile were verified by SHA-256 before
reuse. No code rebuild or production change was made.

The workload remains the permanent `online-holds` case in
`benchmarks/manifest.json` and `benchmark all`: 60 and 300 imports, 16 unique
4 KiB files per import, application readers, and online GC at 5 ms intervals.
This investigation creates no new workload. The full setup is described in
the [previous report](2026-09-10-retirements-quiet.md).

## Acceptance rule

Require five consecutive seconds at or below 15% total external process CPU
before starting a run, with a 90-second waiting limit. Monitor the same limit
throughout each run. Record competing processes and kernel-worker CPU; kernel
workers remain separate because they may perform the benchmark's own I/O.
Accept a timing pair only when both invocations stay within the CPU budget.

The first launch retained the old independent veto on compiler processes. It
timed out before running a benchmark, despite a final sample at only 9.9% CPU.
That separate veto was removed for the subsequent launch: competing processes
are recorded, but eligibility depends on the user's numerical CPU allowance.
Both protocols and outcomes are retained; their samples are not pooled.

## Outcome

The CPU-budget launch completed both 60-import warmups with all correctness
gates passing. The before warmup exceeded 15% during execution; the after
warmup remained within the allowance. Before the first measured invocation,
the host failed to sustain the required interval within 90 seconds. The final
sample showed **69.4% external CPU usage**, dominated by Rust compilation,
Clippy, and another workload.

No measured pair ran and the 300-import stage was not reached. This provides
no performance estimate and cannot confirm or refute the original roughly
5% slowdown. The 15% allowance remains the chosen policy for a future retry;
the blocker here is activity far above that allowance.

Artifacts include the [CPU-budget protocol](2026-09-11-retirements-15pct/cpu-budget/protocol.json),
[runner](2026-09-11-retirements-15pct/cpu-budget/run.py),
[warmup data and host samples](2026-09-11-retirements-15pct/cpu-budget/runs.json.gz),
[blocking sample](2026-09-11-retirements-15pct/cpu-budget/blocked.json), and
[progress log](2026-09-11-retirements-15pct/cpu-budget/progress.log).
The initial process-veto attempt is retained in the adjacent
`initial-process-veto` directory.

Reproduce with the saved binaries and lockfile:

```sh
python3 benchmarks/results/publication-retirements-cpu15-2026-09-11/run.py
```

Use a fresh results directory for another attempt to preserve these records.
The retained runner expects `bin/before`, `bin/after`, and `Cargo.lock` beside it.
