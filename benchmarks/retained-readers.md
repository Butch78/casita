# Retained reader ownership

Run correctness gates before measuring:

```sh
devenv shell cargo test --features experimental --test retained_process_pins --test application_api
devenv shell cargo test --features experimental --lib metadata::pins
```

Run the permanent corpus:

```sh
devenv shell benchmark run retained-readers
# Eight short cases through the aggregate runner, retaining logs and samples:
devenv shell benchmark all --suites retained-readers --profile smoke --output /tmp/casita-retained-readers
# Override repetitions within each standalone case:
CASITA_BENCH_RETAINED_ITERATIONS=100 devenv shell benchmark run retained-readers
```

Each run compares durable owned holds with application `retained_reader()`
sessions at 1 and 32 simultaneous readers, both with and without concurrent GC.
Every iteration drops the session before reading, checks every payload byte,
and verifies that all its readers share exactly one active pin. Each case checks
that protection is released, GC ran when requested, and fsck is clean. JSON lines
report admission p50/p95, opening time per reader, total wall time, ledger revision
changes, and collection passes. Setup and final integrity checks are outside the
wall interval; per-iteration correctness and flush work are inside it.
Local sessions use the existing process-reader ledger. After owner admission,
ordinary snapshot admission and release avoid durable ledger writes; the corpus
checks unchanged durable bytes in warmed process-reader cases without GC.
Writes and remote admission retain their existing durable protection. The existing
`reader-coordination` and `ledger-boundaries` suites cover revision reservations
and ledger rollover thresholds.
