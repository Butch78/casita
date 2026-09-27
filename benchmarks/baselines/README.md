# Published baselines

Release-facing benchmark runs belong here as pairs:

- `REVISION-MACHINE.json` contains authoritative environment metadata, tool
  versions, corpus hashes, commands, raw samples, and aggregates.
- `REVISION-MACHINE.md` is generated from that JSON with
  `benchmark --render-existing REVISION-MACHINE.json`.

After promoting all suite results, regenerate the unified interactive page from
the repository root with `benchmark-dashboard`. The command reads every
checked-in baseline by default and writes `docs/public/benchmarks/index.html`;
pass `--catalog-output docs/public/benchmarks/catalog.json` to publish the
normalized machine-readable catalog beside it.

Only publish a standard-profile run made with at least ten repetitions, both
cache policies, `--require-all`, and `--require-clean`. Keep the interpretation
boundaries generated in the Markdown report beside every result table.
