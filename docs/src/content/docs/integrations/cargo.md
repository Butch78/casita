---
title: Cargo
description: How the experimental Cargo fork stores dependency data and build artifacts in Casita.
---

The experimental Cargo integration adds an `ArtifactStorage` interface inside
Cargo, with filesystem and Casita implementations. Casita remains a generic
repository: Cargo sends local paths through IPC, and Casita imports or restores
their content. It does not gain Cargo-specific lockfile, package-resolution, or
checksum object formats.

:::caution[Experimental fork]
This integration is implemented on the
[`artifacts+casita` branch of the Cachix Cargo fork](https://github.com/cachix/cargo/tree/artifacts%2Bcasita)
and is not part of an upstream stable Cargo release. It requires the fork's
unstable `-Zcasita-storage` flag and the pre-release Casita CLI.
:::

## Enable the backend

Install the Casita CLI, which includes local IPC support:

```console
$ cargo install --path /path/to/casita/crates/casita
```

Select the backend in Cargo configuration:

```toml title=".cargo/config.toml"
[cache]
storage = "casita"
```

Then run the patched nightly Cargo with the unstable feature enabled:

```console
$ cargo build -Zcasita-storage
```

Cargo uses the default per-user Casita repository, normally the operating
system's data directory joined with `casita`. If its local service is not
already listening, Cargo starts:

```console
$ casita --repository <default-data-directory>/casita ipc
```

## What changed in Cargo

The fork makes these Cargo-side changes:

1. `ArtifactStorage` lets Cargo prepare and persist registry archives, indexes,
   extracted sources, Git databases and checkouts, and workspace build state.
   Its filesystem implementation preserves Cargo's usual layout.
2. Cargo's `[cache] storage` selector accepts `"casita"`, gated by the unstable
   `-Zcasita-storage` feature.
3. `CasitaArtifactStorage` implements the same interface through bounded local
   JSON-RPC calls to `casita ipc`. Cargo does not link Casita's async repository
   implementation.
4. The client negotiates protocol version 1 and requires the
   `artifact.checkout` and `artifact.import` capabilities before moving data.
5. Cargo uses writable local checkouts, asks Casita to restore retained content
   before use, and imports updates after Cargo changes them.

This keeps ownership clean:

```text
Cargo resolution, checksums, extraction, and builds
                    ↓
        Cargo artifact-storage adapter
                    ↓
       local artifact.checkout / artifact.import
                    ↓
         generic Casita roots and storage
```

## Stored artifacts

| Cargo data | Behavior |
|---|---|
| Downloaded registry `.crate` archives | Imported individually and restored on demand |
| Registry indexes and extracted sources | Retained under names chosen by the Cargo adapter |
| Git databases and checkouts | Retained under names chosen by the Cargo adapter |
| Workspace target and intermediate build state | Restored into writable checkouts and retained per workspace and layout |

The adapter chooses root names and can use Casita's filesystem, tar, and native
Git importers for the corresponding content. Cargo still owns the meaning of
each archive, checkout, and build output.

Explicit `--target-dir`, `CARGO_TARGET_DIR`, `build.target-dir`, and
`build.build-dir` settings continue to take precedence. When one of those
settings applies, Cargo preserves the explicitly selected filesystem location
instead of silently redirecting it into Casita.

## Lifecycle and recovery

A later Cargo process can restore retained dependency data and workspace
artifacts from Casita. The integration test deletes Cargo's unpacked registry
sources and confirms that an offline build recovers the `.crate` archive from
Casita before extracting it again.

`cargo clean -Zcasita-storage` operates on the private workspace checkout and
imports the now-empty directory. A later build therefore recompiles instead of
recovering artifacts that were present before the clean.

Casita-managed roots do not participate in Cargo's global-cache garbage
collector. They remain subject to Casita's own root and collection model. Until
the integration exposes retention controls, operators should treat that as a
separate lifecycle and storage-budget decision.

## Current limits

- The integration is a fork experiment, not an upstream Cargo compatibility
  promise.
- Its import and restore path is not yet on par with Cargo's filesystem backend
  for performance. Build speed is a requirement before recommending it for
  regular use.
- It stores dependency data and mutable workspace build state; complete
  lockfile/registry evidence bundles are outside this integration.
- It does not reuse compiled artifacts across workspaces.
- IPC is local to the current machine and user. It is not a remote build-cache
  protocol.
- Cargo still owns package resolution, checksum validation, archive extraction,
  freshness decisions, compilation, and diagnostics.
- Casita verifies and retains the resulting filesystem graphs; it does not
  decide whether a Cargo artifact is semantically valid or reusable.

Read [Local IPC](../ipc/) for the framing, capability, and local trust boundary
used by the adapter.
