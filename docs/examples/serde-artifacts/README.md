# Build Serde itself

This fixture backs the homepage illustration of Serde's build files. It does
not build an application that depends on Serde.

Run from the repository root with Cargo, Rust and Python 3.12+ available:

```sh
python3 docs/examples/serde-artifacts/verify.py
```

The script obtains the published Serde 1.0.229 crate (using Cargo's source
archive cache when available), verifies its pinned SHA-256 checksum, extracts
it into a temporary directory, and builds its actual library with
`cargo build --lib --locked`. It records Serde's `.rlib`, `.rmeta`
and `.d` output files in `docs/public/examples/serde-artifacts.json`, including
actual filenames, sizes and SHA-256 equality fingerprints. These fingerprints
are build evidence, not Casita object IDs. No generated binaries are committed.

The script also builds again in the same workspace and asserts that Cargo
reports Serde fresh, recompiles no targets, and leaves the output bytes unchanged.

The default animation follows only `libserde.rlib` through Write and Materialize.
The full graph in the Casita panel shows retention and illustrative sharing
with the supporting artifacts; their writes are omitted from the animation.

The animation proposes Cargo integrating Casita directly: Cargo writes artifact
payload blobs and chunks, publishes verified file and directory objects, and
then publishes a named build root. `target/debug` is a filesystem projection
of that stored metadata. Reads resolve projected paths to file objects and
serve their payload bytes from Casita. Cargo decides freshness using its inputs,
settings and complete build state; the root name is not a build-cache key.
The verification script uses an ordinary Cargo build to obtain real artifact
names and sizes; it does not implement or validate this proposed integration.
It uses schematic chunk boundaries and sharing; no storage savings or actual
chunk counts are claimed. This is an architecture illustration, not a performance
benchmark. Source archive identity and compiler version are recorded; a future
compiler can produce different artifact bytes and sizes.

The main output is `target/debug/libserde.rlib`, whose exact path is asserted
by the verifier. The metadata and dependency files are supporting artifacts.
