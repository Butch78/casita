# Native Rust FSKit bootstrap validation

Validated on 2026-09-11 on an arm64 Mac running macOS 26.6.2 (25G83), as
an ordinary user over SSH. The private Casita runtime was initially absent;
an older Obrador runtime and an enabled FUSE-T module already existed.

The initial driver interrupted extraction after installer checksum verification,
confirmed no final runtime or receipt was published, and retried concurrently
with a real two-mount test. It temporarily removed only the FUSE-T enablement
entry to exercise activation; all original modules were preserved. Bootstrap,
concurrent mounts, explicit reuse/repair, and the sandboxed Obrador build test
passed. No FSKit mounts remained after validation.

The first process after mounting initially spent 923.75 ms rechecking setup.
The recorded settings inode changed, although the settings bytes matched the
original backup exactly. Runtime/helper fingerprints were unchanged. Receipt
schema 2 therefore hashes the small settings file instead of fingerprinting its
inode and timestamps. Actual settings changes still invalidate the receipt.

After that fix, six fresh-process public `ensure()` calls took 0.219–0.300 ms,
including calls after an identical atomic settings replacement, simultaneous
mounts, and sandboxed builds. Warm public calls took 14.1–16.0 ns each over one
million iterations. The temporary-fixture receipt microbenchmark took about
55 microseconds per call, both before and after identical settings replacement.
These are unoptimized Rust test-profile measurements and exclude process launch
and mounting. They are measurements on this host, not latency guarantees.

The fixed native driver asserts that the receipt bytes remain unchanged after
settings replacement and both integration tests. All checks passed. Native
filesystem/setup tests passed (16 tests); the current Linux checkout passed
24 tests, with the performance test ignored by default. The permanent benchmark
passed separately, and its Python runner passed all seven tests.

Reproduce the permanent benchmark in a configured development shell:

```sh
benchmark run fskit-setup-reuse
benchmark all --suites fskit-setup-reuse --profile smoke --output benchmarks/results/fskit-setup-reuse
# On macOS; may install/enable the private runtime:
CASITA_BENCH_NATIVE_FSKIT_SETUP=1 benchmark run fskit-setup-reuse
```

The permanent corpus includes memory reuse, receipt reuse, and receipt reuse
after identical settings replacement, with a no-subprocess correctness gate.
The integration drivers in this directory accept the validation directory,
filesystem test binary, Obrador builder test binary, and setup binary as their
four arguments. Run under Obrador's development environment to provide its
Clang and Apple SDK paths. `native_validate.py` requires no existing Casita
runtime and backs up module settings before activation testing;
`native_validate_fixed.py` operates on the installed runtime.

Raw initial results are in `before-fix/`; fixed results are in `after-fix/`.
The remote isolated source snapshot is
`/Users/hetzner/casita-fskit-validation.CbqbnM`. Source hashes are retained in
`source-sha256.txt`. The user's settings backup remains on the Mac.
