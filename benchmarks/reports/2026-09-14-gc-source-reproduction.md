# Historical GC benchmark source after reader integration

The GC changes were measured before the packed-reader changes later merged from
remote main. Rebase also migrated three test constructors away from the removed
cache-promotion argument. The historical report source hashes therefore refer to
that earlier source, not the integrated tree.

Reconstruct the historical GC source in a separate checkout:

```console
git worktree add --detach /tmp/casita-historical-gc 62ea96d5f167a03233a50c0a97773d0f25ea4ce1
# Use the patch from the current checkout (an absolute path):
git -C /tmp/casita-historical-gc apply /PATH/TO/CURRENT/benchmarks/reports/2026-09-14-pre-reader-gc-source.patch
```

This source-only patch reconstructs the runtime and runnable benchmark code from
commit `34439fb`, before rebase. Copy the report-specific patches from the current
checkout, then follow each report's restoration instructions in this historical
checkout. For exact per-marker measured source, use its measured-source patch;
for older GC experiments, use the per-marker baseline patch first. Keep the same
Cargo.lock and compiler environment when comparing binaries; the lockfile is not
tracked by this repository. Raw reports preserve the compiler and binary hashes.

The historical source patch intentionally contains the old constructor API. It
must be applied to the specified base, not the current packed-reader implementation.
