# Shared immutable enumeration entries — 2026-09-16

## Result

Reusing immutable entry vectors reduces repeated metadata work, but does **not**
resolve the FSKit launch penalty. The same-binary comparison shows little change
at the usual 256-sibling fixture. At 512 siblings binary launch improves by an
observed 8%, while script launch stays near 34 ms. Keep this in the isolated
native evaluation; these results do not justify replacing production fuser.

Medians of three round p50s, milliseconds:

| Metadata siblings | Native script off → on | Native binary off → on | fuser script / binary, on run |
| --- | ---: | ---: | ---: |
| 0 | 13.923 → 13.937 | 7.388 → 7.301 | 5.610 / 6.697 |
| 128 | 19.147 → 18.185 | 9.141 → 9.235 | 4.760 / 7.159 |
| 256 | 24.918 → 24.879 | 12.634 → 12.365 | 5.543 / 8.610 |
| 512 | 34.568 → 34.214 | 18.329 → 16.858 | 7.226 / 9.269 |

Enumeration callbacks per script/binary launch remain 4/2, 4/2, 8/4 and 12/6
across these sizes. Both sides of the previously observed pagination steps are
covered. At 512 siblings, median time inside our callbacks drops from 12.043 to
10.823 ms per script and 8.319 to 6.601 ms per binary. At 256 siblings, script
callback time increases from 6.294 to 6.804 ms, illustrating timing variation.
There are no timed native repository directory reads in either mode.

The direct code-object control also retains the same callback counts. At 512
siblings, native `SecStaticCodeCreateWithPath` goes from 3.612 to 2.924 ms. This
is a diagnostic control, not an additive estimate of system launch overhead.
See the [bundle-discovery investigation](../2026-09-16-native-fskit-enumeration/README.md).

## Change

The old path rebuilt `Vec<Entry>` for every enumeration page, cloned each
entry's metadata and repeatedly locked the node map to resolve IDs. The native
path now returns an `Arc<Vec<Entry>>` cached per immutable directory. Dot entries
are chained onto the list without cloning it. FSKit packing and filename object
construction still happen per callback.

Root and `/views` are mutable namespaces and remain uncached. Publication keeps
working after a cached miss. Disabling the older directory cache disables this
cache too. The fuser adapter continues using its original `entries` path.
No Objective-C objects or content readers are retained in the new cache.

The cache adds retained metadata proportional to visited directory contents,
released with its backend. It has no eviction policy yet, like the prototype's
existing directory cache. Production integration must address memory limits;
the small launch gains here should be weighed against that memory cost.

## Evidence and validation

- Both aggregate sweeps and all eight child runs completed correctness and
  teardown gates, including exact listings without duplicates, bytes, mmap,
  execution, mutation/sandbox denial, publication and independent mounts.
- Every run uses identical source/build/native/fuser binary identities. Paired
  sizes have identical snapshot descriptors. Source receipts match the current
  local implementation, including its Rust regression assertions.
- Each child has 14 cases × three backends × three rounds × ten samples:
  1,260 timed operations per child, 10,080 across both sweeps.
- Ordinary user on Apple M1 / macOS 26.6.2, no root or security-policy changes.
- Cache-off ran first, then cache-on. Backends and cases rotate within runs.
  One-minute load ranged about 2.1–2.7; host script medians were 3.38–3.47 ms.
  These are shared-host observations, not confidence-bounded speed guarantees.
- Three Rust library tests and 41 Python tests pass; formatting and whitespace
  checks pass. Tests assert shared-vector reuse, mutable namespace freshness,
  and independent disable controls. The Mac release extension compiled and ran.

The initial `entry-cache-off*.json` attempt failed safely before mounting because
another builder had an active FSKit volume. It is retained as failure evidence.
After that builder released its mount, `entry-cache-off-retry*.json` and
`entry-cache-on*.json` completed. No unrelated mount was unmounted.

## Permanent controls and reproduction

Both `native-fskit-launch-enumeration-uncached` and
`native-fskit-launch-density-enumeration-uncached` are registered in the manifest
and `benchmark all`. Normal launch/density suites enable entry-vector caching.
The `--enumeration-cache enabled|disabled` switch allows exact build reuse.

From the repository root on the Mac, with the previously documented toolchain:

```sh
python3 -m benchmarks.cli run native-fskit-launch-density-enumeration-uncached \
  --profile standard --repetitions 3 --output results/entry-cache-off-retry.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_launch_density import main
r = json.load(open("results/entry-cache-off-retry-0.json"))
raise SystemExit(main([
    "--profile", "standard", "--repetitions", "3",
    "--enumeration-cache", "enabled",
    "--bundle", r["bundle"], "--server-binary", r["server_binary"],
    "--output", "results/entry-cache-on.json",
]))
PY
```

Run `python3 benchmarks/reports/2026-09-16-native-fskit-entry-cache/summarize.py`
locally to verify matching identities and reproduce launch/callback summaries.

## Next

Prefer measuring the remaining FSFileName construction and packing cost before
adding another cache. Repeated callbacks still impose a launch floor. An
alternative is deeper system-caller tracing to investigate whether supported
filesystem behavior can avoid repeated bundle discovery. Keep fuser as the
production backend while pursuing either path.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
