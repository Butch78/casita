# Casita fuzzing

These targets exercise parsers and validation boundaries implemented by Casita:

- `logical_records`: logical object, root, manifest, and transfer codecs;
- `frozen_formats`: directory, IPLD, and Git-view codecs;
- `casitar_stream`: Casitar prefixes and the streaming state machine under arbitrary read fragmentation;
- `git_owned_parsers`: Casita's native tree/commit/tag link extraction, immutable-view decoding, and ref-name parsing;
- `input_boundaries`: filesystem names, symlink targets, and SSH endpoints.

The harness deliberately does not fuzz Gitoxide's repository, packet-line, hash, or pack implementations. Casita uses those libraries but does not own their parsers. New Casita-owned hostile-input boundaries should be added to the nearest target.

Run a target with nightly Rust and `cargo-fuzz`:

```console
cargo +nightly fuzz run logical_records -- -max_len=262144
```

Keep inputs bounded. When a fuzz failure is fixed, copy the minimized reproducer into `corpus/<target>/` with a descriptive name and leave it there as a permanent regression seed.
