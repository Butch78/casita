# Pruning benchmark source after catalog integration

The pruning and row-ID measurements predate the external catalog and owned
publication changes merged from main before pushing this contribution. These are
historical measurements, not fresh timings of the integrated runtime.

Reconstruct the measured row-ID candidate in a separate checkout:

```console
git worktree add --detach /tmp/casita-measured-pruning c2c6eac0ae6812490101d112e790aeb7b3c0a023
git -C /tmp/casita-measured-pruning apply /PATH/TO/CURRENT/benchmarks/reports/2026-09-14-pruning-measured-source.patch
```

The source patch restores the measured metadata implementation and benchmark
runner. Every source hash in `2026-09-14-rowid-delete-paired.json` was verified
against this reconstruction. The remaining source files come from the specified
base commit. Keep the same compiler and Cargo.lock as the measured environment;
Cargo.lock is not tracked by the repository.

From that reconstructed source:

- For the row-ID candidate, build it directly.
- For the original pruning profile or individual-key baseline, apply
  `2026-09-14-rowid-delete-baseline.patch` from the current checkout.
- For the rejected batched candidate, apply the baseline patch, then
  `2026-09-14-delete-batch-prototype.patch`.

Use absolute paths to the patches in the current checkout. Follow each report's
build and run commands after restoring its source. Do not apply historical source
patches directly to the newer integrated implementation.

The integrated held-GC fixture resolves external catalog references through the
existing packed reader before checking sharded roots and shared bases. This
updates fixture inspection for main's catalog format; historical timings above
still use the preserved original fixture. Runtime collection and metadata checks
were rerun after integration, alongside packed-storage tests and benchmark tooling.
