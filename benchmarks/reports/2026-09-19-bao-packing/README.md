# Bao packing feasibility, 2026-09-19

Packing small sidecars is worth a production prototype. This experiment reduces
local sync calls and improves small-sidecar storage time even after writing a
lookup index. It does not establish an end-to-end import speedup, and it does
not implement a production storage format. Obrador's dependency pin is unchanged.

See [benchmark and implementation boundary](../../bao-packing.md) for the
permanent runner, design constraints, and exclusions. The benchmark is included
in the manifest and `benchmark all`.

## Timing

Criterion mean milliseconds, ten samples per case, debug profile on a shared
host. First run alternates loose then packed; the selected repeat runs packed
before loose. All runs finish compilation before measurement. Confidence
intervals, raw samples, and the executable hash are in [results.json](results.json).

| Sidecars | Payload bytes per blob | Loose first | Packed first | Loose repeat | Packed repeat |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 16385 | 5.80 | 5.84 | | |
| 32 | 16385 | 18.04 | 6.61 | 14.42 | 6.53 |
| 584 | 16385 | 268.85 | 8.24 | | |
| 585 | 16385 | 290.99 | 7.81 | 247.34 | 14.14 |
| 586 | 16385 | 262.02 | 12.08 | | |
| 16 | 1064959 | 9.17 | 10.44 | | |
| 16 | 1064960 | 9.21 | 10.86 | | |
| 16 | 1064961 | 9.06 | 10.64 | | |

The 585 small-sidecar pack occupies exactly 65,536 bytes including footer and
trailer. The 586th sidecar requires a second pack. Each small sidecar contains
64 bytes; the prototype adds 48 footer bytes and 80 index bytes per sidecar,
plus 16 bytes per pack. This exchanges more serialized metadata for fewer
filesystem objects. The larger-sidecar cases do not show a benefit; above the
production paging threshold, this probe still stores raw outboards and does
not measure production page objects.

## Syscalls and correctness

A separate correctness-mode trace of 32 small sidecars counted:

| Sync calls | Loose | Packed |
| --- | ---: | ---: |
| Files | 32 | 2 |
| Directories | 95 | 4 |
| Total | 127 | 6 |

Both modes use `LocalFileSystem.with_fsync(true)`. The packed mode writes one
pack and one lookup index. Original timestamped traces are retained. SIGCHLD
notifications from Criterion's Cargo metadata lookup are excluded from call
counts, not successful sync calls. Counts cover the complete invocation.

All 16 permanent correctness cases passed. Verification reopens storage,
checks every sidecar, validates pack hashes and index/footer ranges, and verifies
range proofs against independently checked blob hashes. All 25 benchmark CLI
and all-runner tests passed. No production code changed, so a full repository
crash campaign was not run. CI was skipped.

## Reproduction

Run from the development shell:

```sh
cargo test --locked --features experimental --bench bao_packing -- --test
# Set BIN to the executable path printed above; the recorded run uses debug.
CRITERION_HOME=/tmp/bao-packing-new "$BIN" --bench
CRITERION_HOME=/tmp/bao-packing-new "$BIN" --bench 'bao_packing/packed-16385/(32|585)$' --save-baseline repeat
CRITERION_HOME=/tmp/bao-packing-new "$BIN" --bench 'bao_packing/loose-16385/(32|585)$' --save-baseline repeat
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o loose.strace \
  "$BIN" --test 'bao_packing/loose-16385/32$'
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o packed.strace \
  "$BIN" --test 'bao_packing/packed-16385/32$'
```

Next: implement packed **outboard root objects**, with a separate versioned
location table in the atomic catalog, loose-read compatibility, and pack-aware
retention and GC. Keep large outboard pages unchanged. Require crash, corruption,
repair, and overwrite coverage before enabling it, then measure the GNU grep
closure. Increasing batch sizes alone would not satisfy those requirements.

All-features/all-targets Clippy, formatting, and diff checks passed. The only
Clippy correction was removal of an unused benchmark import; measured behavior
was unchanged.
