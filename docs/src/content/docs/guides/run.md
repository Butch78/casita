---
title: Run Applications from Casita
description: Import an application tree, select its executable, and run a retained copy.
---

`casita run` takes an existing named directory root, checks out its tree into a
private temporary directory, and starts an executable from that tree. It does
not build the application or fetch a missing root.

For a root already published by a build tool, the command can be as short as:

```console
$ casita run cargo/builds/uv -- --version
```

## Import and run a tree

Install the CLI from a Casita checkout or follow the
[Quick Start](../../getting-started/). This POSIX example makes one executable:

```sh
mkdir -p output/bin
cat > output/bin/hello <<'SH'
#!/bin/sh
printf 'Hello from Casita\n'
SH
chmod +x output/bin/hello
```

```console
$ casita --repository ./cache import ./output --root apps/hello
$ casita --repository ./cache run apps/hello
Hello from Casita
```

Use the same repository for import and run. A compiled application can include
its required libraries and assets in `output/`. The program must also be
compatible with the host's operating system, architecture, and installed
runtime dependencies.

## Choose an executable

Casita searches the tree recursively. On Unix, files need an executable mode
bit; on Windows, candidates need `.exe` or `.com`. If several candidates exist,
`run` lists them and asks for `--bin`:

```console
$ casita --repository ./cache run apps/hello --bin hello
$ casita --repository ./cache run apps/hello --bin release/hello
```

A filename must match uniquely. A relative path selects one exact candidate;
use `--bin ./hello` to distinguish a root-level file. On Windows, `--bin hello`
can match `hello.exe` or `hello.com`. Stored symlinks to executables inside the
tree can be selected, but directory links and links outside the tree are not
searched. Casita does not search your shell's `$PATH` for a candidate.

Put application arguments after `--`. The child inherits the current working
directory, environment, and standard streams. Its exit status becomes Casita's
exit status.

## Shorten root names in a project

Run `casita init` in the project directory, then add these settings to its
`.casita` marker without removing the marker header or workspace UUID:

```toml
[run]
default-scope = "cargo"

[run.scopes]
cargo = "cargo/builds"
go = "go/builds"
```

```console
$ casita run cargo:uv -- --version
$ casita run go:server -- --port 8080
$ casita run uv -- --version
```

The last command expands to `cargo/builds/uv` through the default scope. A
full root path stays literal, and `/uv` bypasses scope expansion to select the
literal root `uv`. Casita finds the closest `.casita` marker in the current
directory or its ancestors. `run` does not add the workspace UUID to root
names. Unknown scopes fail instead of selecting another root.

## Copy or update an application

Sync a named root before running it from another local repository:

```console
$ casita sync --from ./cache --to ./mirror --root apps/hello
$ casita --repository ./mirror run apps/hello
```

Import a new output under the same root to update later runs. A process already
running keeps the exact tree it selected, even if that root changes or
collection runs. Normal completion removes the temporary checkout and releases
its retention hold. The child runs with your permissions; `run` is not an
execution sandbox.

See the [run command reference](../../reference/cli/#run) for exact selection,
signal, and cleanup behavior.
