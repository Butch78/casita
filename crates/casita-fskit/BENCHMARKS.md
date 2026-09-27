# Native FSKit benchmarks

## Memory transport

`benchmark run native-fskit` measures native Rust FSKit against ordinary host
filesystem reads. `native-fskit-portable` uses portable names on both sides.
The raw-name fixture separately checks arbitrary byte names on the native mount;
APFS rejects invalid UTF-8 names, so those are excluded from its host control.

```sh
benchmark run native-fskit --prepare-only --output /tmp/native-prepare.json
benchmark run native-fskit --bundle /path/from/preparation/CasitaNativeFSKit.app --output /tmp/native.json
benchmark all --suites native-fskit,native-fskit-portable --output /tmp/native-all
```

Enable the evaluation extension in System Settings before mounting. Each suite
remains in the permanent manifest and `benchmark all`. Correctness, independent
mount teardown, boundary reads, sample completeness, and latency ratios are
retained. `p50_host_over_native` and `p95_host_over_native` compare with the host;
they are not the old adapter ratios. The host is a different filesystem with
different caching, not an equivalent Rust transport implementation.

Historical adapter results remain under `benchmarks/reports`.

## Repository workloads

Native FSKit reads a real Casita repository; the control reads the original
fixture on the host filesystem. The fixture utility imports and stages roots
and runs direct reader-cache pressure checks. It does not run a mount server.

```sh
benchmark run native-fskit-repository --prepare-only --output /tmp/repository-prepare.json
benchmark run native-fskit-repository --bundle /path/to/CasitaNativeFSKit.app \
  --server-binary /matching/path/casita-repository-fixture --output /tmp/repository.json
benchmark all --suites native-fskit-repository,native-fskit-launch --output /tmp/native-repository-all
```

`--server-binary` retains its historical spelling for the import/fixture utility.
Preparation records matching source/compiler receipts. Enable the evaluation
extension before mounting. Source changes require a fresh preparation.

The permanent corpus retains boundary reads, metadata, execution, first launch,
concurrent workloads, reader capacities 16 and 32 with cases on both sides of
the thresholds, directory density, xattrs, timestamps, and callback tracing.
It also gates publication after negative lookup, immutable bytes, sandboxing,
busy unmount, independent repositories, no TCP listener, and reader release.
All native suites remain registered in `benchmarks/manifest.json` and included
in `benchmark all`. Ratios now compare native with the host. Historical
three-backend measurements remain under `benchmarks/reports`.

See `benchmark run native-fskit-repository --help` for cache and trace controls,
and [the production API](README.md) for app packaging and mounting.

The standalone [bundle scan diagnostic](../../benchmarks/tools/reproduce_bundle_scans.py)
repeats code-object creation without mounting or timing. Its measured equivalent
remains in the permanent launch suites.
