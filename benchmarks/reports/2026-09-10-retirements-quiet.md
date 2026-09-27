# Guarded retirement comparison on current main

This repeats the permanent `online-holds` workload to investigate the roughly
5% import-time increase in the [original comparison](2026-09-09-publication-retirements.md).
Timing pairs qualify only when **both** runs remain within the quiet-host gate.

## Matched artifacts

- Before: `450a633`, immediately before publication-owned retirements.
- After: `b279897`, current main at the start of this investigation.
- Both include the newer local pin journal and reader inventory cache.
- The tracked benchmark source and Cargo manifest are identical. Both builds
  use the same copied `Cargo.lock` with `--locked`; its SHA-256 is in
  [protocol.json](2026-09-10-retirements-quiet/protocol.json).
- The initial fresh-checkout dependency resolution was stopped before producing
  a measured baseline. The locked baseline was then built successfully.
- Cargo initially reused the baseline artifact when switching source checkouts
  in a shared build directory. The after build was explicitly invalidated and
  rebuilt from the main checkout. The final binaries have distinct hashes;
  only these final artifacts enter the comparison.

Both build logs and binary hashes are retained with this report. Binaries and
the exact lockfile are retained locally under
`benchmarks/results/publication-retirements-quiet-2026-09-10/`.

## Protocol and correctness

Use the existing cases registered in `benchmarks/manifest.json` and run by
`benchmark all`: 60 and 300 imports, 16 unique 4 KiB files per import,
application readers, online GC, and the unchanged 5 ms GC interval. This is a
repeat of the permanent corpus, not a new workload or scheduling experiment.

Run on CPUs `8,10,12,14`. Before every invocation, require five seconds with no
competing build/test processes and external process CPU consumption at most 5%
of total host capacity. Allow up to 90 seconds to obtain that quiet interval.
Continue sampling throughout the run, recording kernel-worker CPU separately.
Reject the entire timing pair if either invocation becomes contaminated.

After one warmup per variant and size, seek three clean pairs, alternating
before/after order. Stop after six attempted pairs for a size, or if a quiet
lead-in times out. Retain rejected attempts and do not substitute an isolated
clean run for its contaminated partner.

The benchmark checks sentinel reads, final checkout, fsck, and settlement of
pins, deletion claims, collector ownership, and prune fences. The runner also
checks GC attempt/phase accounting and exact obsolete-object removal: 1,003
objects at 60 imports and 5,083 at 300 imports.

The [retained runner](2026-09-10-retirements-quiet/run.py) expects `bin/before`,
`bin/after`, and `Cargo.lock` beside it. To reproduce using the saved artifacts:

```sh
python3 benchmarks/results/publication-retirements-quiet-2026-09-10/run.py
```

To rebuild, use separate build directories for the two source checkouts (or
explicitly invalidate the Casita package between builds), copy the same saved
lockfile into both, and run this command in each:

```sh
cargo bench --locked --features experimental --bench online_holds --no-run
```

The Linux binary cannot run on the reachable remote ARM macOS host. Moving the
experiment there would require a separate platform build and would not settle
the observed Linux result.

## Outcome: comparison blocked by host activity

Thirteen 60-import invocations completed: two warmups, five full measured
pairs, and the after half of a sixth pair. Every invocation passed all
correctness and accounting gates. Only the after half of the sixth pair was
quiet, so **zero complete pairs qualify for timing analysis**.

The runner then stopped because the final before invocation could not obtain
a quiet lead-in within 90 seconds. The 300-import stage was not reached.
NetworkManager, desktop-shell, and browser CPU consumption repeatedly exceeded
the gate during otherwise quiet-starting runs. The build processes from this
investigation had already exited before measurements began.

These results do not confirm or refute the original roughly 5% slowdown. Do not
compare the lone quiet after run with a contaminated baseline, pool rejected
pairs, or interpret the runner's nonzero exit as a correctness failure. No
production optimization or catalog-check profiling was attempted on the basis
of these timings. Finishing the requested comparison requires a quiet Linux
host; the exact artifacts are ready for that rerun.

The [summary](2026-09-10-retirements-quiet/summary.json) retains each run's
status and descriptive timings without claiming an effect size. Full benchmark
rows and host samples are in [runs.json.gz](2026-09-10-retirements-quiet/runs.json.gz).
The [progress log](2026-09-10-retirements-quiet/progress.log) and
[blocking sample](2026-09-10-retirements-quiet/blocked.json) retain the terminal
failure to obtain a quiet interval.

## User-requested retry

Retried later on September 10 with the exact same binaries and lockfile, verified
against the first attempt's hashes. Results were written to a fresh directory;
the quiet-host limits and acceptance rules were unchanged.

The before warmup obtained a quiet lead-in after 68.6 seconds and passed all
correctness gates. During execution, external process CPU peaked at 18.1% of
host capacity, above the 5% limit. NetworkManager, the desktop shell, and a
transport service were the largest consumers in that interval. The after warmup
then failed to obtain a sustained five-second quiet interval within 90 seconds.
A single post-timeout sample below the limit does not satisfy that requirement.

No measured pair ran, and the 300-import stage was not reached. This retry
provides no new evidence for or against the slowdown. Retained artifacts:
[summary](2026-09-10-retirements-quiet/retry/summary.json),
[full warmup and host samples](2026-09-10-retirements-quiet/retry/runs.json.gz),
[progress log](2026-09-10-retirements-quiet/retry/progress.log), and
[blocking sample](2026-09-10-retirements-quiet/retry/blocked.json).
