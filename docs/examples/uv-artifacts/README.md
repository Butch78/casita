# Build uv itself

This fixture verifies the executable in the homepage animation using uv 0.12.7,
pinned to commit `61291a8ca5477a9ca653f14d2ac5665587c263fa` and a source archive
SHA-256 checksum. It uses the repository's Cargo.lock.

With Cargo, Rust, Python 3.12+ and uv's native build prerequisites available:

```sh
python3 docs/examples/uv-artifacts/verify.py
```

`--work-dir /tmp/casita-uv-example` preserves build files for incremental reruns.
`--source-archive /path/to/source.tar.gz` uses an existing archive, still checking
its pinned checksum. Compilation defaults to four jobs; CARGO_BUILD_JOBS can
override this. The script records the compiler version and actual build outputs
in `docs/public/examples/uv-artifacts.json` and asserts that `target/debug/uv`
is executable and prints the expected version when run with `--version`.
The supporting library and dependency-info file are sibling stored outputs;
the graph is a storage graph, not a compilation dependency graph.

The animation proposes Cargo writing directly to the `cargo/builds/uv` root,
then running it with `casita run cargo/builds/uv -- --version`. The command
prepares the whole output tree and discovers its executable; the animation
focuses on reconstructing uv's bytes from stored metadata and chunks. The fixture
is an ordinary Cargo build: it verifies filenames, bytes and the executable,
not the proposed Casita integration. Chunk boundaries and sharing are
illustrative. This is artifact correctness evidence, not a performance benchmark.
