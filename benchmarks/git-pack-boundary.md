# Zero-delay streaming-boundary controls

`git-pack-boundary` is included in `benchmark all`. It reuses the permanent
local simulated backend and validation from [git-pack-delayed](git-pack-delayed.md),
with one permit and zero read/output delays. It compares serial, batch eight,
and pipeline eight using six rotations/reversals per six rounds. The default
30 rounds give ten appearances per mode in every position. Warmups are excluded.
Both smoke and standard profiles retain every control; repetitions set run length.

The deterministic seed-9 corpora are:

- `small`: 24 distinct 64 KiB blobs.
- `boundary`: 16 small blobs plus 1 MiB minus one byte, exactly 1 MiB, and plus one byte.
- `buffered`: the same boundary corpus with the streaming blob removed.
- `below`, `at`, `above`: a single blob on each side of the streaming cutoff and at it.

Each corpus runs compression levels zero and six. Level zero removes most
compression work while preserving pack framing and hashing. The `above` control
uses the same streaming path in all modes: it never enters either small-object
read loop. Mode differences there cannot establish a read-pipeline cost.
Single buffered objects also remove multi-object read/encode overlap.

Timers cover pack generation only, including selection cloning and sink copying;
fixture setup, warmup, validation and reporting are excluded. The sink accepts
at most 64 KiB per write, and its accepted-write count must match across modes.
All packs must equal a production baseline and pass independent decompression,
exact blob identity/content checks and trailer validation. Every payload must be
read once, all permits released, and every call complete within 30 seconds.
There is no networking, S3, CPU utilization sampling, or admission gating.

```sh
cargo --offline test --release --all-features --lib --no-run --message-format=json
# Freeze the library test executable from the Cargo JSON compiler-artifact record.
python3 -m benchmarks.cli run git-pack-boundary \
  --probe-binary /path/to/frozen/casita-library-test \
  --repetitions 30 --output /tmp/boundary-fresh.json
```

The report and helper log retain every sample, separate warmups, paired wins,
mode medians, binary and source fingerprints. Use fresh report/log paths.
This suite adds experimental controls only; production defaults are unchanged.
