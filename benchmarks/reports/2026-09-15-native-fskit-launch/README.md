# Native FSKit executable-launch latency investigation

## Finding and fix

**Repeated directory metadata reads caused most of the native launch penalty.**
Warm native executable launches fetched repository directories four times;
direct script launches fetched them eight times. No file blobs were reopened.
Running the script as `/bin/sh script` avoided these directory reads. Changing
Python's wait mode or using `posix_spawn` did not remove them.

The native evaluation backend now caches complete metadata for visited immutable
directories. Both successful and unsuccessful lookups, plus enumeration, use
that cache. The mutable root and `/views` remain live, so cached misses cannot
hide newly published views. The cache holds metadata rather than content readers
and drops with the volume. It grows with visited directory contents; a production
memory/eviction policy remains future work.

This is a fix in the **native prototype**, not a change to production fuser or
`FilesystemView` caching policy.

## Same-binary comparison

[Cache off](cache-off.json.gz) and [cache on](cache-on.json.gz) both completed all gates
and cleanup. They have identical snapshot, source, build identity, native
extension/host hashes and fuser helper hash. Only a marker in each private
evaluation repository selects the native directory-cache policy; fuser ignores it.

Three rounds, 10 launches per case/backend/round: **540 timed launches per run**.
Six cases on native, production fuser/FUSE-T and the host fixture. Each run has
54 launch sample rows and 18 native/fuser comparisons, plus first-touch and
first-execution observations. Backend and case order rotate within each run;
cache-off ran before cache-on. Times below are medians of the three round p50s.
The fuser and host columns come from the cache-on run.

| Case | Native cache off | Native cache on | fuser | Host |
| --- | ---: | ---: | ---: | ---: |
| Direct script, timed wait | 69.365 ms | 19.256 ms | 5.848 ms | 4.741 ms |
| Direct script, blocking wait | 77.003 ms | 19.107 ms | 5.872 ms | 4.787 ms |
| `/bin/sh script`, blocking wait | 4.907 ms | 4.835 ms | 5.644 ms | 4.799 ms |
| Native executable, timed wait | 38.343 ms | 9.743 ms | 7.357 ms | 3.055 ms |
| Native executable, blocking wait | 39.454 ms | 9.442 ms | 7.045 ms | 3.193 ms |
| Native executable, `posix_spawn` | 36.373 ms | 9.878 ms | 6.113 ms | 2.071 ms |

The timed-wait cases improved **3.6× for scripts and 3.9× for the executable**.
All 30 launches in each corresponding uncached native case observed exactly
eight or four directory fetches; every cached launch observed zero. Every timed
launch in these controls observed zero blob opens. This makes repeated metadata
work a stronger explanation than cold executable data.

Before caching, median measured directory time per timed-wait launch was 37.0 ms
for scripts and 19.6 ms for the executable; after caching it was zero. Popen
construction was only a few milliseconds; most delay was after Popen returned.
Directory counters include the async repository call's elapsed time, so they
are not CPU-time profiles and must not be added to wall time.

Python documents that timed POSIX waits may poll; both timed and blocking controls
were therefore retained. On this Python build the default path did not use
`posix_spawn`; the explicit control did, as verified by instrumenting Popen's
actual dispatch. Neither difference explains the repeated repository work.
See [Python subprocess documentation](https://docs.python.org/3/library/subprocess.html).

## What was ruled out and what remains

- The original missing-name hypothesis was too narrow. `._filename` probes occur
  during initial reads, but their counters do **not** increase on warm launches.
  The expanded diagnostic also observes no other missing-name increase per launch.
  Do not attribute the four/eight reads to repeated AppleDouble misses.
- Caching substantially reduces latency but does not eliminate the gap: native
  remains about 3.3× fuser for direct scripts and 1.3× for the executable in the
  timed-wait controls. The interpreted-script control is close to host speed.
- No system security policy was bypassed, no root tracing was used, and no other
  user's mounts/builds were stopped. The cache preserves real `._` files rather
  than hard-coding their absence.
- This remains a shared Apple M1/macOS 26.6.2 host, UID 501, with unrelated builds
  and load around 5. Absolute values vary. `decision_eligible` stays false.
- This fixture has hundreds of sibling files. We have not established how the
  remaining launch cost scales with directory size, or identified which macOS
  service triggers every directory callback. Do not claim a code-signing or FSKit
  kernel bug from these counters alone.

## Reproduction and permanent corpus

```sh
python3 -m benchmarks.cli run native-fskit-launch-uncached --profile standard \
  --repetitions 3 --output /tmp/launch-off.json
python3 -m benchmarks.cli run native-fskit-launch --profile standard \
  --repetitions 3 --output /tmp/launch-on.json
python3 benchmarks/reports/2026-09-15-native-fskit-launch/summarize.py \
  /tmp/launch-off.json /tmp/launch-on.json
```

Both suites are registered in the manifest and `benchmark all`; Linux records
explicit skips. Standard has 10 launches per case/round; smoke has three. They
reuse all repository correctness, sandbox, mutation, cached-miss publication,
independent mount and teardown gates. Every launch checks stdout and exit status.
The suite-level deadline protects blocking waits and partial reports are retained.
See [setup instructions](../../../casita-fs/native-fskit/BENCHMARKS.md#repository-workloads).

Retained exploratory reports: [initial instrumentation](before.json.gz),
[initial cache](after-initial.json.gz), and [prioritized sidecar-name tracing](probe-names.json.gz).
Those are not the same-binary A/B pair; source hashes identify each revision.

An [additional enumeration-tracing attempt](tracing-registration-blocked.json.gz)
compiled but was blocked by the registration guard because an unrelated build
had an active FSKit mount. It took no measurements and is not a successful run.
That additional instrumentation was removed; the final implementation's source
identity exactly matches the completed cache-on/cache-off pair. The remaining
callback-level investigation is explicitly pending.

Validation: 36 Python tests, three Rust library tests, native release builds and
mounted gates passed. The Rust integration test verifies no repeated directory
reads, real `._` file visibility, stable IDs/listings, publication after a miss,
independent lifetimes, and the uncached regression control.

## Next

Keep the cache fix. Profile remaining enumeration/attribute construction and
path resolution during direct exec, with directory-size controls and a quiet
host. Alternatively, prioritize production fuser's metadata overhead while
keeping the native backend experimental. Do not change how applications invoke
scripts merely to make the benchmark faster.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
