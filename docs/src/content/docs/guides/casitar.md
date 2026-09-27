---
title: Import a Casitar Archive
description: Restore a portable, verified Casitar closure archive through the generic importer interface.
---

Rust examples on this page use the `experimental` Cargo feature and
`casita::experimental`. For the supported built-in workflows, see the
[Library guide](/library/).

Casitar archives carry a complete, canonical closure: payloads, immutable
records, and one or more source root declarations. Importing verifies the
entire stream before atomically publishing destination-owned root names. It
does not recreate the source repository namespace.

## CLI

`import` detects a Casitar file from its header. Select `-i casitar` for
standard input. Supply an explicit destination mapping: repeat `--casitar-root` once
for every archive root, in the canonical header order, or use
`--casitar-root-prefix` to map them to `PREFIX/0`, `PREFIX/1`, and so on.

```console
$ casita --repository ./cache import release.casitar \
    --casitar-root releases/current
$ casita --repository ./cache import -i casitar - \
    --casitar-root-prefix releases/2026-08
```

Destination names must be absent by default. `--casitar-replace` replaces them
only when their preflight values have not changed before publication. The
common `--root` option is intentionally not accepted for this importer.

The importer accepts `-` for standard input and uses finite stream limits.
Use `--casitar-max-archive-bytes`, `--casitar-max-payload-bytes`,
`--casitar-max-total-payload-bytes`, `--casitar-max-payloads`, and
`--casitar-max-records` to tighten or override them for a deployment.

`archive import` remains available for its JSON report and its historical
archive-specific flags. `import -i casitar` is the common importer command and
uses only `--casitar-*` options.

## Rust

Open the bounded stream, choose destination names, and pass the reusable
`CasitarImport` value through `Importer`. The destination vector is in the
same order as the roots in the archive header.

```rust
# async fn example() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
use casita::import::{CasitarImport, Importer as _};
use casita::experimental::{
    CasitarReader, CasitarRootConflictPolicy, CasitarStreamLimits, Repository, RootName,
};

let repository = Repository::local("./cache").await?;
let input = tokio::fs::File::open("release.casitar").await?;
let reader = CasitarReader::open(input, CasitarStreamLimits::default()).await?;
let report = CasitarImport::from_reader(reader, [RootName::try_from("releases/current")?])
    .with_conflict_policy(CasitarRootConflictPolicy::RequireAbsent)
    .import(&repository)
    .await?;

println!("published at {}", report.destination_revision);
# Ok(())
# }
```

Use `ReplaceIfUnchanged` only when replacing a root is intentional. A failed
import never publishes any mapped destination root, though verified unrooted
data may remain available for collection.

See [Portable Casitar Archives](../../design/casitar/) for the wire format and
[Import Semantics](../../concepts/imports/) for the shared publication model.
