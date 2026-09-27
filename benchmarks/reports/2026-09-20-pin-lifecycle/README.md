# Pin lifecycle tracing, 2026-09-20

This change adds debug spans around mutation preparation, pin acquisition,
catalog admission/synchronization, and asynchronous pin release. The release
span wraps the actual background future. It changes no admission, release,
publication, or durability decisions.

Baseline: `5791357997e932d6e6b7ee210bba1710288f9328`. Candidate: the commit
containing this report. The same existing six-path NAR closure benchmark uses
Obrador `56c1780`; no new workload or performance threshold is introduced.
The existing registered `nar_import` corpus and closure correctness gates remain
unchanged. Binary identity and observations are in `results.json`; raw traces
are retained in Obrador's `benchmarks/pin-lifecycle-2026-09-20`.

The original 91 calls outside blob staging, directory staging, and path
registration split into 48 during mutation setup, 37 during publication, and
six without an existing lifecycle span. With the new spans, the fresh trace
accounts for every one of its 86 calls outside writer stages:

| Observed lifecycle interval | Calls |
| --- | ---: |
| Mutation preparation | 36 |
| Initial pin acquisition | 6 |
| Mutation catalog admission | 6 |
| Publication | 37 |
| Pin release | 1 |

Publication includes 31 calls during pack-index preparation and six elsewhere
inside publication. Setup is still 48 calls in total. `before_mutation` checks
pending metadata reclamation and disk pressure before admitting the staging
pin; this makes maintenance preparation the next focused investigation. Inspect
its reclamation work and safe opportunities to amortize it before changing
collector fences or deletion claims. The six acquisition and six admission
calls are a smaller alternative.

These are temporal partitions, not causal attribution. Background release can
overlap foreground work. Twelve further ledger calls overlap release spans
inside path registration. Added spans can retain their parents until background
cleanup finishes, changing interval boundaries; scheduling also changes admission
groups. The 91-to-86 difference is not an optimization claim. Trace elapsed time
is not a throughput measurement.

## Profiler correction

Tracing CLOSE events carry the context active at final drop, which may be a
background task without the original import parent. The profiler previously
required that parent, so the first analysis of the fresh trace wrongly treated
registration calls as outside writer stages. It now uses complete recorded
writer lifetimes, clipped to each import window, and independent lifecycle
intervals. Nested/overlapping phase names remain explicit. Both levels reconcile
exactly for all 12 old/new import windows; the six imports in the fresh trace
have non-overlapping windows.

## Validation and reproduction

All 11 pin-runtime tests, four lease tests, and 41 NAR tests passed. Formatting
and diff checks passed. The six-path trace checks canonical NAR contents after
reopening and five warm registrations. Obrador's profiler has six focused tests,
including a regression for writer CLOSE events without the import parent.

```sh
cargo test --lib metadata::pins::runtime::tests
cargo test --lib metadata::lease::tests
cargo test --lib nar::tests
cargo clippy --all-features --all-targets -- -D warnings
RUST_LOG='casita=debug,obrador_core=debug' \
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o trace.strace \
  BINARY /tmp/obrador-real-nar-seed-20260918 \
  /nix/store/sacz532zgiacvg7mva9v6gbfmyw427i3-gnugrep-3.12 OUTPUT_DIRECTORY 1 \
  > run.log 2> trace.jsonl
```

Use the binary identified in `results.json` and Obrador's updated
`benchmarks/profile-nar-closure.py`. This extends an existing benchmark's tracing;
it does not add a separate temporary performance probe.
