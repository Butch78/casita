# Filesystem import reuse

`benchmark all` includes `filesystem-reuse --profile standard`. Each generated
child directory contains a distinct 64-byte file. The smoke profile uses
1, 14, 15, 16, 17, and 64 children. The standard profile also includes
511, 512, 513, 1024, 2047, 2048, 2049, 4095, 4096, and 4097 children.
`--directories` selects other counts.

These cases cover the 16-directory staging window (including the root), the
1024-entry walk page (two entries per child), and the 4096-object publication
batch. Warm imports stage only directories; forced rereads also stage files,
so their publication boundaries differ. All cases run the same correctness
gates below.

```sh
benchmark run filesystem-reuse --casita-bin target/debug/casita \
  --profile standard --output /tmp/filesystem-reuse.json

benchmark run filesystem-reuse --casita-bin target/debug/casita \
  --source /path/to/disposable-project/target \
  --repository /path/to/isolated-casita-repository \
  --output /tmp/filesystem-reuse-project.json
```

The source is read only. Output and repository paths must be outside it. The
runner copies and hashes the supplied binary and retains raw output, JSON traces,
commands, load averages, restored trees, and generated fixtures. An existing
repository is optional; benchmark roots are retained under a unique
`benchmark/filesystem-reuse/` prefix. No root or source is removed.

Each case primes the cache, then measures an untraced warm import, a traced warm
import, and a traced forced reread. The forced reread controls for cache reuse;
the untraced sample exposes tracing overhead. All imports must return the same
canonical tree identity. Unix warm traces must show every regular file as a
cache hit, zero cache misses, and no blob staging. Restored paths, file bytes,
executable bits, and symlink text must match the source; the source inventory
must remain unchanged. Any failed gate leaves `complete: false` and a failed
sample. Full source/restoration inventories are outside the measured commands.

Trace summaries retain counts and accumulated seconds separately for spans and
pin-journal phases. Spans include child spans, concurrent spans overlap, and
`journal_append_sync` is part of `journal_flush`. These totals must not be added
as a wall-clock breakdown. Development and release binaries, host load, and
tracing levels must be controlled when comparing performance.

## Cargo tree investigation

The Riff target from the Cargo storage investigation contains 3,060 regular files
and 1,416 directories. A preliminary cached import showed all 3,060 cache hits
and no misses, yet took 4.2s with tracing enabled. Directory staging accounted for
2.9s of accumulated span time, and 1,104 journal flush events accounted for 2.17s
of accumulated phase time. These figures overlap.

Previously, `MutationSession::import_paths_inner` staged directories serially, calling
`stage_directory` and ultimately `BlobStore::put_slice` for each one. The payload
write path durably protects resources before writing. This produces many small
journal updates even when every file payload is cached.

Directory construction now collects completed directories within each bounded
walk page. Identities are computed in post-order before payload writes finish,
and up to 16 calls to `stage_directory` run concurrently. Ordered buffering
retains child-before-parent publication, including when a page crosses a
publication batch boundary. Every write still uses the existing write scope
and durable protection path. This lets existing pin protection and journal
group commits combine work without changing pin durability or GC coordination.

The permanent case repeated this measurement at 2.96s untraced and 3.47s traced,
again with 3,060 cache hits, zero misses, and zero blob staging. Directory staging
accounted for 2.25s and 1,103 journal flushes for 1.46s (overlapping). Forced reread
took 12.48s with 3,060 blob stages and 1,791 journal flushes. Restored contents
matched the source. A generated tree with only 64KiB of file data across 1,024
child directories took 3.50s for a cached import, confirming a directory-count
cost independently of the large Riff outputs. These are development-binary
measurements on a shared host, not controlled latency budgets.

The [bounded staging investigation](reports/2026-09-22-directory-staging/README.md)
records before/after measurements, all concurrency/page/publication boundary
cases, late controls for host variability, correctness checks, and reproduction
commands. In the late controls, cached Riff import fell from 3.25s to 2.01s
with journal flushes falling from 1,098 to 130. The 1,024-child median fell from
2.32s to 0.92s, with 1,047 versus 92 flushes.
