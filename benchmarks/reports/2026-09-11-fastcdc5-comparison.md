# FastCDC 3 versus 5: correctness passed, timing unavailable

No speedup or regression is established. Quiet-host admission failed before
either version's timed tar matrix could start. The workstation continued to
run other builds; those processes were left running. The available SSH host
was macOS and cannot run these Linux binaries or the `/proc` activity guard.
The 120-second attempt recorded 116 activity intervals and the 180-second
retry recorded 161. Every interval observed competing build processes.

The [raw report](2026-09-11-fastcdc5-comparison.json) retains both attempts,
all waiting samples, executable fingerprints, full Cargo lockfiles, compiler
details, and the exact shared benchmark sources used for the comparison.

| Build | Commit | FastCDC | Release tar correctness |
|---|---|---|---|
| Baseline | `1ee5d7997fbae1e2bdf84f6acdf9722618e0eeb9` | 3.2.1 | 16/16 passed |
| Candidate | `843497a9fa7833d8661fb46cda362dc23bb27840` | 5.0.0 | 16/16 passed |

Both builds use Rust 1.96.0, the same release settings, fixtures, and benchmark
source. Every Cargo lockfile package entry other than FastCDC is identical.
Each case verifies the canonical archive root, published root, counts, logical
bytes, and full payload readback. The permanent matrix covers small-file counts
0, 1, 15, 16, 17, and 256, plus large and mixed archives, at concurrency 1 and 16.
It remains registered in `benchmarks/manifest.json` and included in `benchmark all`.

The comparison runner now accepts separate baseline and candidate binaries,
alternates their execution order between repetitions, and uses fresh Criterion
directories. Paired summaries require every matrix to pass correctness and
host-activity gates. A timeout, partial pair, or contended run produces no
paired speed claim. Compiler detection now also covers Clippy and additional
linker/compiler names; normal benchmark runs clear stale profiling controls.

Review validation passed: 211 Python harness tests, all-features/all-targets
Clippy, formatting, and hash-input correctness checks for all 28 boundary
fixtures and all 1,650 committed regular-file occurrences at the candidate
revision (118,198,434 logical bytes). These correctness scans are not timings.

## Reproduce on a quiet Linux host

From this repository, enter its development environment with Rust 1.96.0 and
run the following. It restores the recorded lockfiles and exact shared harness,
builds each variant once, copies each executable before the next build, and
runs the correctness gates before timing. The original attempts used quiet
timeouts of 120 and 180 seconds; the command below allows 180 seconds.

```sh
python3 - <<'PY'
import json, os, pathlib, shutil, subprocess, sys, tempfile
report = json.loads(pathlib.Path('benchmarks/reports/2026-09-11-fastcdc5-comparison.json').read_text())
work = pathlib.Path(tempfile.mkdtemp(prefix='casita-fastcdc-compare-'))
environment = dict(os.environ)
for key in ('CASITA_BENCH_PERF_CONTROL', 'CASITA_BENCH_PERF_ACK', 'CASITA_TAR_REVERSE'):
    environment.pop(key, None)
binaries = {}
for variant, build in report['builds']['variants'].items():
    checkout = work / variant
    subprocess.run(['git', 'worktree', 'add', '--detach', str(checkout), build['commit']], check=True)
    for name, content in report['shared_source_contents'].items():
        (checkout / name).write_text(content)
    (checkout / 'Cargo.lock').write_text(report['cargo_lock_contents'][variant])
    command = ['cargo', 'bench', '--locked', '--features', 'experimental',
               '--bench', 'tar_import', '--no-run', '--message-format=json']
    built = subprocess.run(command, cwd=checkout, env=environment, text=True,
                           stdout=subprocess.PIPE, check=True)
    rows = [json.loads(line) for line in built.stdout.splitlines() if line.startswith('{')]
    executable = next(row['executable'] for row in rows
                      if row.get('reason') == 'compiler-artifact'
                      and row.get('target', {}).get('name') == 'tar_import'
                      and row.get('executable'))
    binaries[variant] = work / f'tar-{variant}'
    shutil.copy2(executable, binaries[variant])
    subprocess.run([str(binaries[variant]), '--test'], env=environment, check=True)
subprocess.run([sys.executable, '-m', 'benchmarks.tar_compare',
                '--baseline-binary', str(binaries['baseline']),
                '--binary', str(binaries['candidate']), '--output', str(work / 'paired'),
                '--repetitions', '4', '--quiet-timeout', '180'], env=environment, check=True)
PY
```

The runner records activity throughout measurement as well as during admission.
Sampled quietness cannot prove physical isolation. Retain the complete accepted
pairs and their environment before interpreting any candidate/baseline ratios.
