# Bounded pre-mutation catalog reclamation

Successful advisory catalog sweeps now defer another attempt for at most 15
subsequent mutation starts on the same local repository handle and its clones.
The next start attempts cleanup again. Previously, historical catalog pins
could keep the durable reclaim marker set and trigger a sweep on every start.
The first pending sweep on a new handle, busy retries, and failed retries
remain immediate. New garbage created during deferral shares the same bound.
Reopen, vacuum, ordinary collection, and disk-pressure policy retain their
existing paths. Collector ownership, publication fencing, durable deletion
claims, and cancellation-safe cleanup are unchanged.

This defers garbage reclamation, not persistence of imported data. The bound
counts mutation starts, not seconds or bytes: an idle handle can retain garbage
until another start, reopen, or vacuum. Independent handles have independent
counters. Each successful attempt arms a countdown; concurrent admissions consume it
atomically and may cause extra sweep attempts.

## Measurements

Both closure binaries use Casita baseline
`5fd5b6223b73b133c2bbed9f0fe9b4e186851390`; the candidate additionally applies
`candidate.patch`. Obrador's benchmark is revision
`56c1780291fe6b72f7e4e694b8af41c81a3817ed`, and its trace profiler is revision
`d925706d55feeb4e6438cfb618e742a617ca2d6f`.

The six-path gnugrep closure contains 1,035 files, 26 symlinks and 40,486,910
payload bytes. Both runs passed canonical NAR SHA256 checks after reopening
every path and five warm closure registrations. The archived closure results
retain each path's NAR identity. Every per-import sync partition reconciles.

| Observed pin-ledger syncs during cold imports | Before | After |
| --- | ---: | ---: |
| Pre-mutation preparation outside writer spans | 36 | 6 |
| All import stages | 469 | 406 |

The total reduction is 63 syncs, or 13.4%. Span lifetimes provide temporal
correlation, not causal attribution of shared ledger work. The narrower
preparation row excludes maintenance overlapping file staging, directory
staging or registration. Scheduling can change group sizes. These are one
paired observation, not a stable exact sync budget or a throughput claim.
Raw JSON traces, strace output, profiles and executable hashes are retained.
Instrumented timings and concurrent builds make elapsed times unsuitable for
a performance claim. Deferred cleanup still has to be paid for later.

The distinct-root synthetic sequence retains every import report, independently
scrubs every root, and rejects association hits:

| Retained NAR imports | Before syncs | After syncs |
| --- | ---: | ---: |
| 15 | 151 | 125 |
| 16 | 161 | 133 |
| 17 | 171 | 141 |
| 32 | 322 | 264 |

The permanent `nar_import_sequence` Criterion group additionally covers 18 and
19 imports, accounting for the initial import before a reclaim marker exists.
It runs through the existing `nar_import` target in `manifest.json` and
`benchmark all`. Timings exclude repository creation, independent scrub and
final collection. The library test prints sync counts without asserting an
exact value sensitive to scheduling.

## Reproduction

```sh
devenv shell cargo test --lib local_catalog_reclamation
devenv shell cargo test --lib repeated_nar_intake_reports_maintenance_syncs -- --nocapture
devenv shell cargo test --bench nar_import
devenv shell cargo bench --bench nar_import -- nar_import_sequence
```

For the closure comparison, build the same Obrador checkout twice with local
Cargo patches for `casita` and `casita-fs`, first at the baseline, then with
`candidate.patch`. Both Linux-only benchmark checkouts change the macOS feature
name in `obrador-build/Cargo.toml` from `darwin-fuse` to `darwin-fskit`, solely
to permit Cargo dependency resolution. This does not migrate Obrador's macOS
mount API, so its published dependency pin is intentionally unchanged.

```sh
devenv shell cargo build --release -p obrador-core --example nar-closure-bench
# SOURCE is an existing Obrador repository containing ROOT. OUT must be fresh.
RUST_LOG='casita=debug,obrador_core=debug' \
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o trace.strace \
  target/release/examples/nar-closure-bench "$SOURCE" "$ROOT" "$OUT" 1 \
  > closure.log 2> trace.jsonl
python3 benchmarks/profile-nar-closure.py trace.jsonl profile.json --syscalls trace.strace
```

Use root `/nix/store/sacz532zgiacvg7mva9v6gbfmyw427i3-gnugrep-3.12` to reproduce
the saved closure. The profiler also reads the archived gzip files directly.

## Validation

The full library suite passed: 720 tests, 41 ignored. It includes process-death
matrices for catalog rebasing, external catalog migration, packed publication
and paged overwrites. The maintenance regressions cover the exact interval,
shared clone state, a start without pending work, historical-reader release,
busy publication retry, vacuum and reopen during deferral.

All 47 permanent NAR benchmark correctness cases passed, as did strict
all-features/all-targets Clippy, formatting and whitespace checks. The final
synthetic run includes the 18/19 boundary cases. Validation output is retained
in `validation.log.gz` and the synthetic logs.

The next integration task is migrating Obrador's macOS build-input reader to
Casita's native repository-backed FSKit mount, then adopting this revision.
The old API accepted custom content readers, including Obrador's relocated
input view; changing only the feature name is insufficient.
