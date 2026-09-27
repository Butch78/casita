---
title: Add a New Importer
description: Turn a new input into verified Casita objects, with an optional CLI command.
---

An importer reads one kind of input and publishes objects through a repository.
Start with an existing object format when it fits. A new parser for text, an
archive, or a remote feed can store ordinary blobs or canonical directories.
If the input needs its own object keys or links that the receiver must verify,
[define an object format](../custom-formats/) as well.

The Rust `Importer` trait is available to library users. Implementing it does
not add a command to `casita import`; that requires a separate CLI change.

## Define the contract

Before writing data, decide:

- Which input forms are accepted, and how large they may be.
- How paths, links, duplicates, and malformed records are handled.
- Which object format represents the result and which root, if any, retains it.
- What the caller receives after publication and what can remain after failure.

Keep meaning that must survive sync in an `ObjectFormat`. An importer's parser
can reject bad input, but a destination can enforce only the format's
verification rules. For filesystem input, validate paths and link targets
before making directory entries. Stream an archive or use handle-rooted access
to a source tree rather than extracting untrusted input first.

## Reuse a built-in format

This example accepts bounded, newline-terminated UTF-8 text and stores it as an
ordinary blob.
It uses the `experimental` feature for Casita's `async_trait` re-export; the
`Importer` trait and `BlobImport` request themselves are part of the supported
library API.

```rust
use casita::{
    import::{BlobImport, Importer}, ObjectKey, Repository, RootName,
};
use casita::experimental::async_trait;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

struct LineImport<R> {
    reader: R,
    root: RootName,
}

#[async_trait]
impl<R: AsyncRead + Unpin + Send> Importer for LineImport<R> {
    type Report = ObjectKey;
    type Error = io::Error;

    async fn import(self, repository: &Repository) -> Result<ObjectKey, Self::Error> {
        const MAX_BYTES: u64 = 64 * 1024;
        let mut bytes = Vec::new();
        self.reader.take(MAX_BYTES + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > MAX_BYTES
            || std::str::from_utf8(&bytes).is_err()
            || !bytes.ends_with(b"\n")
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid line"));
        }

        let key = repository
            .import(BlobImport::new(bytes.as_slice(), self.root))
            .await
            .map_err(io::Error::other)?;
        Ok(key)
    }
}
```

`BlobImport` stages the bytes, verifies their blob identity, and publishes the
root. The line rule above applies only during this import. A receiver verifies
the blob's bytes and identity, but does not enforce the line rule. Put that rule
in a custom `ObjectFormat` if every repository must enforce it.

## Publish a graph

For several linked objects, use the lower level API under
`casita::experimental`. Keep one `MutationSession` from the first payload
write through root publication. It protects staged bytes from collection.

Stage content with `stage_blob_reader` or `stage_blob`, canonical directories
with `stage_directory`, and registered objects with native keys through
`stage_object` or `stage_existing`. Each call returns a sealed `StagedObject`
after format verification. Publish those values rather than manually
constructed `ObjectRecord` values.

Stage children before parents. Publish bounded intermediate batches with
`publish_unrooted`, then use `publish_rooted` once the complete closure exists.
A failed import may leave collectible unrooted objects, but does not move the
root to a partial graph. If replacement depends on an earlier root value, use
`publish_if_roots_match`. Use `publish_at_revision` when the entire publication
must match one observed repository revision.

## Add a CLI command if needed

To expose an importer in Casita's CLI, add it to the CLI's `ImporterKind` list
and `casita import` dispatch. Parse the location, limits, and root options,
then call the reusable Rust importer. Keep verification and publication in the
library.

Give flags for each importer distinct names, such as `--tar-max-entries`.
Automatic detection should use a small, bounded structural probe rather than a
filename extension. Standard input needs an explicit `-i` unless the CLI can
replay the bytes it probed. Document the required Cargo feature, accepted input
forms, limits, root behavior, output, and failures. A library-only importer
needs no CLI command.

Test valid and malformed input, each size limit, unsafe paths or links when
relevant, conflicting entries, object identity and links, interrupted input,
and final root publication. Test CLI parsing separately if you add a command.

See [Import Semantics](../../concepts/imports/) for the shared lifecycle and
the [Experimental Rust API](../../reference/experimental-rust-api/#publication)
for staging and publication methods.
