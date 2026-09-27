# Running and retaining benchmarks

From the repository root, enter `devenv shell` and use:

```console
benchmark list --groups
benchmark all --groups native-git
benchmark all --suites transfer-holds --repetitions 3
benchmark all
```

`all` defaults to the smoke profile. It prints a unique result directory under
`benchmarks/results/`; `--output DIRECTORY` still selects an explicit, fresh
directory. Groups come from `suite_id` in the manifest. `--groups` and `--suites`
are alternative selectors. With neither, every registered entrypoint is selected.
Platform restrictions and external-corpus requirements still apply. Skipped or
failed cases prevent the completion ledger from claiming a complete run.

The runner asks Cargo to build only the selected targets. Cargo checks source,
toolchain, dependency and build-option freshness on every invocation. Executable
bytes are retained by SHA-256 under `benchmarks/artifacts/`, with hard links from
each run's `bin/` directory. Outputs on another filesystem use copies. Builds
never modify these retained executables. Commands and compiler messages remain
with each run; `artifacts.json` records source paths and hashes, which are checked
again at completion. `--bin-dir` uses the same retention mechanism.

Measurements, logs, Criterion samples and provenance stay in the result
directory. Child-process temporary directories and the runner's RustFS fixture
use a uniquely marked directory under `benchmarks/work/`. Explicit suite-specific
paths and workloads that manage their own storage can still use other locations.

## Inventory and archives

```console
benchmark inventory --output benchmarks/archives/inventory.json
benchmark archive benchmarks/results/OLD_RUN --output benchmarks/archives/OLD_RUN.tar.gz
```

Inventory reports logical file bytes, extension totals, approximate content
categories, symlinks and textual
references from reports/baselines. Hard links are counted at each path, so logical
bytes can exceed physical disk usage. References are review hints, not proof that
an unreferenced file is disposable. Categories use extensions, Git paths and
executable signatures; they are also review hints. Inventory never follows
directory symlinks.

Archives retain the entire source tree, including logs, fixtures and binaries.
They preserve symbolic links as links, without following external targets. The
command hashes regular files, creates the archive, then reads it back and checks
every file hash and link. A neighboring `.tar.gz.json` contains the verification
manifest and archive SHA-256. Existing destinations are rejected. Source files
are never removed by `archive`. Use a quiescent run for an archive; a failed
verification must not be used as grounds for deleting the source.

## Cleanup

```console
benchmark clean
benchmark clean --apply
```

The default is a JSON preview. Cleanup considers only marked work directories
whose matching execution ledger says the runner finished. Unmarked historical
directories, interrupted runs, results, reports, baselines, Cargo build caches
and retained executables are left alone. Before applying removal, cleanup
archives and verifies the entire scratch directory under `benchmarks/archives/`
so diagnostic traces are retained too. Archive failure leaves the scratch tree
in place. Preview byte counts are logical source sizes, not promised net savings
after compression. Archives are never automatically pruned.
