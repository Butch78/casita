---
title: Quick Start
description: Install Casita, store a directory, restore it, and copy it to another repository.
---

Casita requires Rust 1.94.1 or newer. From a source checkout, install the CLI:

```console
$ git clone https://github.com/cachix/casita.git
$ cd casita
$ cargo install --path crates/casita
```

These commands use the repository checkout as the example project. Run them from
its top-level directory.

## Store a directory

Create a local repository at `./cache`, then import Casita's source directory
under the name `projects/demo`:

```console
$ casita --repository ./cache init
$ casita --repository ./cache import ./crates/casita/src --root projects/demo
$ casita --repository ./cache root ls
```

The import prints a directory object key, such as `casita.directory.v1:...`.
The root keeps that directory and its contents available across restarts.

## Inspect and restore it

Use the key printed by `import` in place of `casita.directory.v1:...`:

```console
$ casita --repository ./cache tree list casita.directory.v1:...
$ casita --repository ./cache checkout casita.directory.v1:... ./restored
```

Checkout requires an absent or empty destination. By default, it also creates
an `auto/checkout/...` root that retains the restored graph. Use `--no-root` if
you do not need that extra root.

## Copy it to another repository

```console
$ casita sync --from ./cache --to ./mirror --root projects/demo
$ casita --repository ./mirror root ls
```

The destination verifies the graph before setting its root. For SSH sources,
path selection, and retry behavior, see [Synchronization](../guides/sync/).

## Check and collect

```console
$ casita --repository ./cache fsck --audit-only
$ casita --repository ./cache gc --dry-run
```

`fsck --audit-only` checks integrity without repairing physical data. The GC
dry run shows what collection would remove. To collect, run `gc` without
`--dry-run`. Named roots and active operations protect their reachable data.

Without `--repository`, `casita init` attaches a project directory to Casita's
per-user repository using a `.casita` workspace marker. See the
[CLI reference](../reference/cli/#repository-and-filesystem-commands) if you
prefer that workflow.
