# Historical notes before removal of fuser

Commands below describe the former implementation and are not current entrypoints.

# Native FSKit with real Casita storage

`native-fskit-repository` compares the Rust extension with production
`CasitaFuse`, vendored fuser and FUSE-T's FSKit backend. Both read the same
immutable snapshot through `FilesystemView`, the same instrumented
`ContentReader`, and `Repository::local`. Native now reuses up to 32 immutable
file readers; the production fuser frontend still opens readers per read.
Native inode metadata caching and OS/adapter cache policies remain different;
this measures the complete implementations, not isolated IPC cost.

Native reader reuse holds one seekable stream per cached inode, with an LRU
limit of 32 resident slots. A per-file mutex protects seek/read position without
holding the global cache lock during I/O. In-flight reads can temporarily retain
evicted streams; unrelated files remain independently readable. Failed reads
discard the affected stream, and flush clears cached holds before the repository
barrier. `--reader-cache disabled` creates a private `disable-reader-cache`
marker for the same-binary baseline. `--reader-cache-capacity 16` compares the
previous limit against the default 32 using the same executable. Fuser's read path is unchanged.

`native-fskit-first-launch` creates fresh mounts for unprepared, executable-read
and Security-code-object preparation trials. Setup and preparation are timed
separately from first and immediate second execution. Content/listing validation
runs afterward, with all ordinary lifecycle and sandbox gates still required.
`native-fskit-first-launch-uncached` disables native reader reuse. Both are in
`benchmark all` and include direct backend pressure cases at 15/16/17 files,
covering the original 16-reader boundary independently of the OS page cache.
Mounted workloads additionally cover the new 31/32/33-reader boundary.
See [first-launch results](../../benchmarks/reports/2026-09-18-native-fskit-first-launch/README.md).

The permanent `native-fskit-workloads` and `native-fskit-workloads-uncached`
cases extend this to real GNU awk, sort and gzip executables. They cover shared
and distinct paths with 1/8/15/16/17/31/32/33 concurrent processes, then immediately repeat
each batch. Input is supplied through stdin; shared libraries remain on the host.
Distinct paths cycle through three tool binaries and have independent file IDs.
Tool sources are selected by `CASITA_WORKLOAD_AWK`, `CASITA_WORKLOAD_SORT` and
`CASITA_WORKLOAD_GZIP`; wrapper scripts are rejected and multicall command names
are preserved. Reports fingerprint the binaries and input and retain all mounted
correctness and teardown gates. See [workload results](../../benchmarks/reports/2026-09-18-native-fskit-workloads/README.md).

The `native-fskit-workloads-read-trace` case enables `--trace-read-ranges` for
bounded per-mount inode/offset/length counters and service time. It requires
complete callback coverage with no dropped ranges or read errors. The
`callbacks` array records completed regular-file callbacks as
`[calls, backend_ns, copy_ns, reply_ns, errors]`; copy time includes releasing
the temporary byte vector. Diagnostic reads and argument validation are
excluded. Active snapshots may omit replies still in progress; unmount must
account for every backend read. Concurrent durations are service sums, not a
partition of wall time. The ordinary
workload case leaves tracing disabled. See [read-range analysis](../../benchmarks/reports/2026-09-18-native-fskit-read-ranges/README.md).

Native lookups now retain complete metadata for each visited immutable directory.
This serves both present and missing names without rereading the repository;
the mutable root and `/views` namespace are excluded. The cache holds metadata,
not open content readers, and is released with its volume. Memory grows with
visited directory contents, not with arbitrary absent names. Production memory
limits/eviction remain future integration work.

Native enumeration also reuses an `Arc<Vec<Entry>>` for each visited immutable
directory. This avoids rebuilding IDs, cloning per-entry metadata, and taking a
node-map lock for each entry on every page. Root and `/views` remain uncached;
dot entries are chained onto the shared list without cloning it. Cached lists
contain metadata only and are released with the backend. This adds retained
metadata proportional to visited directory contents; production eviction remains
future work. The fuser control continues using its existing entry path.

`native-fskit-launch-enumeration-uncached` disables only this new vector cache.
`native-fskit-launch-density-enumeration-uncached` applies that control to all
four sibling counts. Both are in the manifest and `benchmark all`; the normal
launch/density suites enable it. Disabling the older repository directory cache
also disables vector reuse, preserving that control's semantics.
The [same-binary sweep](../../benchmarks/reports/2026-09-16-native-fskit-entry-cache/README.md)
shows modest savings in larger directories, with little launch improvement at
256 metadata siblings. This does not resolve the native launch penalty.

`native-fskit-launch-density-phases` measures enumeration setup, FSFileName
construction, packer calls and filename release separately. Enable these with
`--enumeration-timing detailed`; the default `basic` mode avoids per-entry clock
reads. `native_enumeration_phases` stores five totals per directory/attribute/
initial-cookie key: setup ns, filename ns, pack ns, drop ns and packing attempts.
Attempts include the rejected entry when a page fills. Setup covers directory
retrieval and dot-entry preparation; per-entry item creation, loop bookkeeping
and timing overhead remain in the surrounding callback total.
The [phase attribution report](../../benchmarks/reports/2026-09-18-native-fskit-phases/README.md)
contains same-binary detailed/basic sweeps and explains their instrumentation
and shared-host limits. Packing is the largest measured phase.

`native-fskit-launch-profile` samples this run's validated native extension PID
as the same ordinary user, after correctness/lifecycle warm-up. The default
profile case uses 512 metadata siblings, ten rounds and a 20-second stack sample.
It writes `.stacks.txt` and `.sample.log` beside the JSON report. Failed sampling
fails the run; interrupted sampling is terminated before mount cleanup. This
case is in `benchmark all` and is for attribution rather than latency claims.
The [native packer profile](../../benchmarks/reports/2026-09-18-native-fskit-packer/README.md)
captures FSFileName data access and Objective-C ownership work inside packing.

`--filename-construction bytes` switches enumeration to FSKit's copying
`initWithBytes:length:` initializer. The default `data` mode retains
`nameWithData:` and its temporary NSData. Other filename uses are unchanged.
Before exposing a volume, both constructors must preserve empty, ASCII,
dot-underscore, invalid UTF-8, Unicode and 255-byte names after the source
buffer is overwritten and freed. These are constructor checks; the repository
suite still retains its separate mounted-byte and listing gates.
`native-fskit-launch-density-filename-bytes` is the permanent density control,
including both sides of the previously observed pagination steps.
The [constructor comparison](../../benchmarks/reports/2026-09-18-native-fskit-filenames/README.md)
found cheaper construction but no consistent launch improvement, so data mode
remains the default.

`--volume-capabilities explicit` declares case-sensitive names, 64-bit IDs,
fast statfs and unavailable root timestamps. It does not declare persistent
IDs because IDs are mount-local. The launch suite reads the VFS capability
bits back and checks case-sensitive lookup before timing. The permanent
`native-fskit-launch-capabilities` case is included in `benchmark all`.
The [capability comparison](../../benchmarks/reports/2026-09-18-native-fskit-capabilities/README.md)
found unchanged scan counts, so `minimal` remains the default.

The native extension receives a macOS 26+ `FSPathURLResource` for the repository
and starts/stops its security-scoped access. Repository lock/database writes stay
within that scope. Our host and extension are Rust using Objective-C bindings to
Apple frameworks. There is no application TCP bridge or broad filesystem
entitlement. Setup and mounting run under the ordinary user.

Each volume owns its repository/runtime, namespace and counters. Native root
inode 2 and `views` inode 3 are separate from the production FUSE numbering.
Publication accepts only pre-staged immutable descriptors under that repository;
it is an evaluation interface, not a production publication API. Unload checks
for outstanding backend owners, flushes repository cleanup, and releases the
backend before acknowledging completion. The fuser helper joins the session,
drops its publisher and flushes the same repository. Its final force-unmount
matches existing `PersistentMount`; native final unmount is ordinary.

## Reproduce

Use macOS 26+, Rust/Cargo, Apple SDK/linker, Python 3 and Casita's existing
rootless FUSE-T setup. The retained Mac
[environment](../../benchmarks/reports/2026-09-15-native-fskit/macos-environment.sh)
documents a working Nix toolchain without Xcode installation.

```sh
python3 -m benchmarks.cli run native-fskit-repository --prepare-only \
  --output /tmp/repository-prepare.json
python3 casita-fs/evaluations/native-fskit/enable.py --repository
python3 -m benchmarks.cli run native-fskit-repository --profile standard \
  --repetitions 5 --output /tmp/repository-standard.json
```

Alternatively activate the repository extension in System Settings. The helper
preserves other enabled modules and refuses to restart the user agent while an
FSKit mount exists. Optional `CASITA_NATIVE_TARGET_DIR` reuses Cargo artifacts;
each prepared app/helper still gets source/build/binary receipts. Reuse requires
both `--bundle` and its matching `--server-binary` from the preparation report.
Ad-hoc signing is development evidence only; distribution remains untested.

## Permanent workload and gates

The suite is in `benchmarks/manifest.json` and `benchmark all`. It covers zero
and one byte, and both sides of 4 KiB, 16 KiB, 64 KiB, 128 KiB and 1 MiB;
directory listing, 256-file stat traversal, open/read/close, held-descriptor
reads, 1/4/16-worker reads, shell scripts and a Rust executable.

Fresh-mount first-touch samples precede correctness; import has already warmed
backing storage, so these are not cold-disk measurements. First execution is
separate. Correctness and another warm-up after lifecycle probes precede five
alternating timed rounds. Each regular case has 30 samples per round; execution
has three. Exact byte/size, nested traversal, symlink, mmap and EOF checks gate
the comparison. Mutation denial, sandbox denial, cached-miss publication,
distinct repository canaries, busy-unmount refusal and cleanup must also pass.

Directory calls and blob opens are comparable counters on both adapters. The
native `reads`/`bytes` counters instrument native callbacks only; fuser zeros
are not evidence of no reads. Counter files have unique inodes to avoid stale
cached observations. Host load and active processes accompany every round.

Timed rounds have a 1,200-second deadline (`--timeout-seconds` to override).
Expiry fails the comparison, preserves partial results and runs mount cleanup;
it is never counted as a successful sample. The retained Mac reports predate
this guard, added after observing prolonged fuser enumeration.

Repository filenames are portable, including `byte-ascii`; the report's
`portable_names: false` describes the unused memory-backend Cargo feature, not
the repository fixture. The separate `native-fskit` raw-name suite retains the
known fuser/FUSE-T invalid-UTF-8 regression. Do not waive it with this suite.

`comparison_complete` requires all paired cases and teardown. `decision_eligible`
remains false: shared-host timings, evaluation publication and ad-hoc signing
cannot authorize a production migration.

## Local validation

### Launch regression controls

Immutable item timestamps now default to one second after the Unix epoch,
matching the production frontend's store metadata. Zero timestamps caused
repeated parent-directory scans during warm launches. The same-binary
`--item-timestamps zero` control creates a private `zero-timestamps` marker;
the permanent `native-fskit-launch-zero-times` case is included in `benchmark all`.
The runner verifies atime, mtime and ctime through VFS before timing.
Both modes are supported by the density suite across 0/128/256/512 siblings.
See [the timestamp investigation](../../benchmarks/reports/2026-09-18-native-fskit-directory-controls/README.md).

The launch suite also measures plain directory listings and public CFBundle
discovery separately from Security code-object creation. The standalone
`reproduce_bundle_scans.py /path/to/executable --iterations 100` repeats the
Security call using only Python's standard library on an existing mount. It is
a trace reproducer, with no mount setup or latency measurement. The permanent
launch suite supplies its timed counterpart and correctness/lifecycle gates.

```sh
python3 -m benchmarks.cli run native-fskit-launch-uncached --profile standard \
  --repetitions 3 --output /tmp/launch-off.json
python3 -m benchmarks.cli run native-fskit-launch --profile standard \
  --repetitions 3 --output /tmp/launch-on.json
```

Both are permanent suites in `benchmark all`. The uncached runner creates a
`disable-directory-cache` marker inside its private evaluation repositories;
this affects only the native adapter's directory metadata cache, not fuser or
file-content caching. The same compiled backend supports both modes. Reports
identify the configuration and record source/binary identities.

Each run performs the repository correctness/lifecycle gates, then three rounds
of six launch cases plus eight path/xattr/code-object controls on native, fuser
and the local host fixture (10 samples each
with the standard profile, three with smoke). Cases and backend order rotate.
Cases separate direct/interpreted scripts, timed/blocking waits and explicit
`posix_spawn`. Successful stdout and exit status gate every sample. The observed
Python spawn path and separate Popen/communicate durations are retained, with
repository counters outside the timed interval. The global deadline protects
blocking waits. Missing, duplicate, failed or unmatched controls fail comparison.

Directory/blob-open duration counters apply to both adapters; missing-name
diagnostics apply only to native. Common fixture sidecar probes are grouped to
leave room for executable-related names in the bounded diagnostic report.

See [launch results](../../benchmarks/reports/2026-09-15-native-fskit-launch/README.md).

### Enumeration and extended-attribute controls

Plain enumeration now avoids allocating item attributes unless requested.
`native-fskit-launch-eager` retains eager allocation for same-binary comparisons.
The native counters include enumeration calls, packed entries, callback time
and item allocations. Plain listings include dot entries, and the listing
correctness gate rejects duplicate names.

The path controls compare open/close with open/`F_GETPATH`/close and validate the
returned path. Xattr controls check missing-attribute reads, empty lists and
mutation denial. Explicit native xattr callbacks passed the Mac gates but did
not remove launch-time enumeration and made direct xattr queries slower.
Emulation remains the default. Use
`native-fskit-launch-explicit-xattrs` for the opt-in comparison.

Additional controls check libc `realpath`, `getattrlist` name/full-path results,
and successful `SecStaticCodeCreateWithPath` object creation. The first three
cause no native directory scans; code-object creation reproduces four callbacks
at 256 metadata siblings. A rootless stack sample identifies CoreFoundation's
bundle-layout and Info.plist discovery scans in that control. Attribution to
the system caller during an actual launch remains an inference.

`native-fskit-launch-density` and
`native-fskit-launch-density-explicit-xattrs` sweep 0, 128, 256 and 512 metadata
siblings, requiring identical binaries across sizes. All controls are registered
in the manifest and `benchmark all`.

The [enumeration investigation](../../benchmarks/reports/2026-09-16-native-fskit-enumeration/README.md)
retains the successful eight-case allocation comparison (before xattr controls),
the ten-case xattr comparison, and the directory-density results. Recorded
source identities distinguish the tested revisions. The density corpus covers
both sides of the observed enumeration pagination steps.

### Tests

```sh
cargo test --locked --manifest-path casita-fs/evaluations/native-fskit/Cargo.toml \
  --features repository,fuser-baseline --lib
python3 -m unittest benchmarks.tests.test_native_fskit \
  benchmarks.tests.test_native_fskit_repository benchmarks.tests.test_native_fskit_launch benchmarks.tests.test_all \
  benchmarks.tests.test_cli
```
