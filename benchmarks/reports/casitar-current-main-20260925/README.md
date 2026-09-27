# Casitar record-pin batching on current main

Worktree: `/home/domen/dev/casita-current-main-20260925`, branch `codex/casitar-current-main`. The worktree was moved from `/tmp` after all runs finished; raw reports preserve the original execution paths, and `artifacts.json` records both original and current binary paths.

Comparison base: `e50179d3f73be902d4d42befb86ac9b6c4eebb43`.

Main already batches publication, co-admits loose blob/chunk resources, and
coalesces concurrent pin requests. Casitar still awaits `stage_existing` once
per record. The candidate protects each bounded group before verifying records
individually, allowing the existing protection cache to avoid per-record durable
updates. Root publication and all verification gates remain in place.

Both builds include `baseline.patch`: opt-in phase timing and temporary verify
repository cleanup. The latency run disables phase timing for both binaries.
The candidate additionally contains record batching and its correctness tests.
`Cargo.lock.txt` is shared by both builds. Toolchain: rustc 1.96.0.

Build each overlay in a worktree at the exact base, using this lockfile:

```sh
cargo build --locked --release --features cli --bin casita
```

Copy the executable before building the other overlay. Exact binary hashes,
source patches, commands, raw results, activity samples, and summary accompany
this report. The four-repetition comparison alternates variants at 4,096 files
and uses the user-authorized 40% external CPU ceiling, with no compiler veto.

## Validation

- 6 Casitar import tests, 3 pin-protection tests, and 2 CLI pipe/cleanup tests passed.
- 406 Python tests ran successfully, with 2 skips.
- All-features Clippy passed for the library and CLI with `-D warnings`.
- Rust formatting and `git diff --check` passed.
- 36 scaling smoke samples and 24 paired boundary samples passed all correctness gates.
- Fully reused imports at 254 files used 260 durable pin appends on main and 7 on the candidate; at 256 files the counts were 262 and 8.

The test-only build emits an existing dead-code warning for `sync::sliced::fuzz_decode` without the fuzzing feature. All-features Clippy is clean.

`rejected-1/` retains the initial attempt: external CPU peaked at 85.679%, with 156 of 222 measured intervals above 40%. Its six baseline samples are excluded. The retry starts after our validation builds have finished, retaining the same 40% policy and binaries.

`port.patch` contains the complete runtime and permanent-corpus port onto the base revision, excluding reports. It passed reverse-application validation against this worktree. `candidate.patch` is the runtime/test overlay used by the candidate build; `baseline.patch` is the common instrumentation/cleanup overlay.

`rejected-2/` retains the second attempt, started after our validation builds had finished. External CPU peaked at 84.070%, led by an unrelated `wasm-opt` process; 36 of 162 measured intervals exceeded the ceiling. Its six baseline samples are excluded. Activity was back to 10–28% in its final ten samples before the next retry.

`rejected-3/` retains the third attempt: one of 138 measured intervals reached 40.348%, led by a `rustc` process. The other intervals were below 40%, but the case is still excluded under the same ceiling. That compiler exited before the fourth attempt started.

## Outcome

No accepted 4,096-file latency comparison is available. All four attempts failed the unchanged 40% CPU ceiling during the first baseline case. No rejected sample contributes to a speedup claim. This supersedes any inference that the earlier September 15 baseline measures current main.

| Attempt | Peak external CPU | Intervals over 40% | Measured intervals |
| --- | ---: | ---: | ---: |
| 1 | 85.679% | 156 | 222 |
| 2 | 84.070% | 36 | 162 |
| 3 | 40.348% | 1 | 138 |
| 4 | 85.331% | 115 | 240 |

The accepted boundary correctness/diagnostic run confirms that current main's concurrent pin coalescing and loose blob/chunk batching do not eliminate serial Casitar record protection:

| Files | Records | Main pin appends | Candidate pin appends | Reduction |
| --- | ---: | ---: | ---: | ---: |
| 254 | 255 | 260 | 7 | 97.3% |
| 256 | 257 | 262 | 8 | 96.9% |

These are journal-operation counts, not latency speedups. Boundary and smoke timings ran without host isolation and are not used for latency conclusions. Compiler activity alone never rejected a case; actual CPU samples above 40% did.

The four-round command is retained in `command.json`. Rerun it into a new output directory when the host can stay below 40%; then audit the complete result with `summarize.py` and the recorded artifact hashes. The source port and all checks are complete. The remaining work is an accepted paired timing run, after integration into main. Testing the combination with PR #81 is an optional follow-up; its journal resource deltas are a separate optimization.

## Integration validation

The port was rebased without conflicts onto `e9baab4270a3da36878e7a6f66c7b794e787ca09` for the requested direct push to main. On that base, the 3 protection tests, 6 Casitar import tests, 40 focused Python tests (1 skip), Rust formatting, and the required all-features/all-targets Clippy hook passed. The benchmark binaries and measurements above remain tied to `e50179d`; no latency result is claimed for the rebased code.
