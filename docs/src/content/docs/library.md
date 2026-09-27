---
title: Use Casita as a Rust Library
description: Open a repository, import data, and use the supported Rust API.
---

The supported API starts with `casita::Repository`. Open a persistent local
repository with `Repository::local(path).await`, or use `Repository::memory()`
for temporary data. The handle has no backend type parameters.

## Import and restore a directory

```rust
# mod example {
use casita::{Repository, RootName};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let repository = Repository::local("./cache").await?;
    let name = RootName::try_from("projects/demo")?;
    let key = repository
        .import(casita::import::FilesystemImport::new("./project", name))
        .await?;

    repository.checkout(&key, "./restored").await?;
    repository.flush().await?;
    Ok(())
}
# }
```

`import` verifies the tree, stores it, and points the named root at its
directory object. `checkout` needs an absent or empty destination. The Rust
call does not create a checkout root. The import root above retains the graph.

Other built-in requests in `casita::import` handle blobs, tar, Casitar, Git
(with `git`), and copies between repositories. See the
[import guides](../guides/filesystem/) and [Rust API reference](../reference/rust-api/).

## Read a blob

```rust
# mod example {
use casita::{Repository, RootName};
use tokio::io::AsyncReadExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let repository = Repository::local("./cache").await?;
    let name = RootName::try_from("blobs/greeting")?;
    let key = repository
        .import(casita::import::BlobImport::new(&b"hello"[..], name))
        .await?;

    let mut reader = repository.open(&key).await?.ok_or("missing blob")?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    drop(reader);
    repository.flush().await?;
    assert_eq!(bytes, b"hello");
    Ok(())
}
# }
```

`open` returns a reader for a stored payload. It retains the data it needs
against collection until dropped. Drop readers before a final `flush()` and
await that flush before shutting down the runtime.

## Names, copies, and maintenance

| Task | API |
| --- | --- |
| Inspect a name or object | `root`, `roots`, `object` |
| Set or remove a name | `set_root`, `compare_and_set_root`, `remove_root` |
| Copy a named graph | `import(CopyImport::new(...))` |
| Export an archive | `export_casitar` |
| Check or collect | `fsck`, `preview_collection`, `collect`, `vacuum` |

`set_root` verifies the complete graph and replaces a name.
`compare_and_set_root` replaces it only when its current value matches the
expected value. A mismatch returns `false`. `remove_root` likewise requires
the expected current key.

`CopyImport` reads one retained source root and copies its complete graph to
the destination. The destination verifies it before setting its own root.

`fsck()` returns an integrity report. `is_healthy()` checks for reachable
corruption; `is_clean()` requires no findings at all. Errors expose `kind()`
and `retry_disposition()`. See [operations](../guides/operations/) and
[errors](../reference/errors/) for details.

## Optional storage and advanced APIs

`Repository::s3(bucket, prefix, writer)` uses the optional, experimental
`s3` storage profile. It uses standard AWS credentials. For a runnable
upload and download example, see
[`s3_sync.rs`](https://github.com/cachix/casita/blob/main/crates/casita/examples/s3_sync.rs).

Custom object formats, backends, transfer sources, and tuning APIs live under
`casita::experimental` with the `experimental` feature. They may change
between revisions. Start with [custom formats](../guides/custom-formats/) or
the [experimental API reference](../reference/experimental-rust-api/).
See [Cargo features](../reference/cargo-features/) for dependency settings.

## Test remote application workflows

These integration tests start a local S3-compatible server, so they need no AWS
account or pre-existing bucket:

```console
$ devenv shell cargo test --features s3 --test s3_application_api
$ devenv shell cargo test --features s3 --test s3_multi_owner
$ devenv shell cargo test --features s3,experimental --test s3_multi_owner_recovery
```

They exercise uploads, downloads, retained readers, collection, and recovery
across processes. The [multi-owner guide](../guides/s3-multi-owner/) explains
the shared-repository model.
