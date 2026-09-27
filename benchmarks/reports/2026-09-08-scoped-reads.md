# Object-scoped reads, 2026-09-08

`Repository::open` now retains the requested object's dependency closure rather
than every object visible in its metadata snapshot. The stream owns this
protection and can outlive the repository handle. Full snapshot and owned
snapshot holds retain their original semantics.

Admission reuses the existing pin-before-validation sequence, metadata resource
and catalog protection, and pinned payload backend. A closure pin replaces the
snapshot-generation scope for this path. No snapshot is exposed to callers of
the scoped operation; the returned immutable record and stream concern only the
requested key.

Two regression tests failed before the change because GC removed zero unrelated
objects instead of one. They now pass on memory and local storage. A reader
opened after garbage already exists allows collection of that garbage, remains
readable and seekable after its repository handle is dropped, and releases its
own object on drop. A directory reader preserves its child objects for checkout
until the reader is dropped. The S3 application workflow also verifies that an
independent collector removes preexisting unrelated garbage while the stream
remains readable.

The benchmark defaults to `CASITA_BENCH_READER_SCOPE=object`; set it to `snapshot`
to run with full snapshot holds. The experimental engine's `open_payload` uses
the same implementation as the supported `Repository::open`. `reader_open`
measures admission and opening the payload stream, excluding reading the bytes.
Older `reader_admission` measurements excluded opening the stream and are not
directly comparable.

All four default scenarios passed with 30 imports of 16 unique 4 KiB files:

| Workload | Files/s | Writer admission p95 (ms) | Ledger revisions/file | Background GC reported removals | Final cleanup removals |
| --- | ---: | ---: | ---: | ---: | ---: |
| Imports | 7.92 | 150.43 | 2.44 | 0 | 493 |
| Imports + reader | 7.52 | 317.83 | 3.43 | 0 | 493 |
| Imports + GC | 10.56 | 320.60 | 2.63 | 68 | 374 |
| Imports + reader + GC | 8.90 | 646.73 | 3.51 | 493 | 0 |

The combined case reported one successful GC pass, 107 Busy attempts, and no
other retryable GC errors. Its background collector removed all 493 obsolete
objects before final cleanup. In-flight passes can finish after the import timer
stops, so this is not evidence of uninterrupted reclamation during continuous
writes. The deterministic tests establish the narrower claim: a live object
reader no longer retains unrelated garbage. Revision conflicts remain possible.
Reported background removals include only successful passes; retryable failures
may make partial progress (the GC-only row consequently undercounts removals).

These are single-run diagnostics on the same shared Linux/btrfs machine as the
prior reports, with changing load. They do not establish a throughput speedup.
Every scenario passed final recovery, pin cleanup, fsck, and checkout byte
verification. Raw output, source diff, executable hash, and environment metadata
(captured after the run) are retained locally under
`benchmarks/results/online-holds-scoped-2026-09-08/`.

Validation passed: 169 selected library tests (74 repository, 32 pin-ledger, 63
packed-storage), seven application API tests, two S3 application tests, and five
online-GC/cancellation integration tests. The snapshot-mode benchmark smoke run
also passed. All-features/all-targets Clippy, the default-feature build check,
and all-features rustdoc with warnings denied passed, as did formatting and
diff checks.
