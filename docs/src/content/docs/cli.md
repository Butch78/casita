---
title: CLI
description: Common commands for storing, copying, and checking objects.
---

Install the command from a [source checkout](../getting-started/):

```console
$ cargo install --path crates/casita
```

The examples use `--repository ./cache` to keep data in a directory you choose.
Without that option, Casita uses its per-user data directory. `sync` uses
`--from` and `--to` instead.

## Store and read files

```console
$ casita --repository ./cache import ./project --root projects/demo
$ casita --repository ./cache root ls
$ casita --repository ./cache tree list casita.directory.v1:...
$ casita --repository ./cache checkout casita.directory.v1:... ./restored
```

Replace the example key with the one printed by `import`. `tree list` shows a
directory's direct entries; `checkout` restores it to an absent or empty path.
See [filesystem import](../guides/filesystem/) for cache and checkout behavior.

## Keep or release a graph

```console
$ casita --repository ./cache root set releases/current casita.directory.v1:...
$ casita --repository ./cache root rm releases/current
$ casita --repository ./cache gc --dry-run
$ casita --repository ./cache gc
```

A root name retains the complete graph it points to. Removing a name makes
unshared data eligible for collection; it does not immediately delete bytes.
Checkout creates its own `auto/checkout/...` root unless you pass `--no-root`.

## Copy a graph

```console
$ casita sync --from ./cache --to ./mirror --root projects/demo
```

The destination verifies received objects before moving the selected root.
For SSH and S3 endpoints or copying a single path, see the [sync guide](../guides/sync/).

## Check integrity

```console
$ casita --repository ./cache fsck --audit-only
$ casita --repository ./cache fsck --dry-run
$ casita --repository ./cache fsck --source ./verified-replica
```

The first command only audits. The second previews safe physical repairs; the
third may repair physical data from an independently verified local replica.
See [operations](../guides/operations/) before restoring or backing up a repository.

## Other workflows

- [Run a stored application](../guides/run/)
- [Import tar](../guides/tar/), [Git](../guides/git/), or [Casitar](../guides/casitar/)
- [CLI reference](../reference/cli/) for every option and its exact behavior
