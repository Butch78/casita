# SlateDB capability experiment

Run `cargo run --release --manifest-path benchmarks/experiments/slatedb/Cargo.toml`.
This isolated workspace deliberately keeps SlateDB out of Casita's dependency
graph. It checks atomic publication, snapshot isolation, and whether opening
another writer preserves Casita's competing-writer contract.

The single timing is diagnostic startup evidence over an in-memory object
store. It is not a storage performance comparison: this probe has no format
verification, retention holds, physical payloads, or garbage-collection adapter.

Upstream contracts: [single-writer architecture](https://slatedb.io/docs/get-started/introduction/)
and [Rust API](https://docs.rs/slatedb/latest/slatedb/struct.Db.html).
