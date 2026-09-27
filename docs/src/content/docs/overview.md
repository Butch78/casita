---
title: What is Casita?
description: Understand what Casita stores and when to use it.
---

Casita stores immutable objects and the links between them. Give an object graph
a name, and Casita keeps everything reachable from that name. You can copy the
graph to another repository, verify it there, and collect data that no name
needs anymore.

It works with filesystem trees, native Git objects, IPLD blocks, and custom
formats. Each format keeps its own identity. For example, a Git commit keeps its
Git object ID. Casita provides the storage and lifecycle shared by those formats.

:::caution[Pre-release]
The CLI and Rust API may change. These docs follow the current `main` branch.
Review [local repository recovery](../reference/local-repository/) before using
Casita for important data.
:::

## What Casita does

| Need | Casita's role |
| --- | --- |
| Store an object | Verify its bytes, identity, and links before publication. |
| Keep a graph | Point a named root at its first object. |
| Move a graph | Copy it to another repository, which verifies the received objects. |
| Reclaim space | Collect objects that no root or active operation retains. |

Casita stores the bytes separately from object identity. Chunking and
compression can change without changing a Git ID, directory key, or other
logical object key.

Casita does not decide which package to build, which Git branch to trust, or
who may access a repository. The application using Casita makes those decisions.
See [responsibilities](../concepts/responsibilities/) for the boundaries.

## Where to start

- [Quick Start](../getting-started/) stores and restores a directory.
- [CLI](../cli/) shows common commands.
- [Rust library](../library/) shows the supported application API.
- [Concepts](../concepts/) explains objects, roots, verification, and collection.
- [Reference](../reference/) covers exact syntax and advanced behavior.

The optional S3 storage profile and custom backends are experimental. See
[Cargo features](../reference/cargo-features/) for build requirements.
