#!/usr/bin/env python3
"""Repeatable end-to-end benchmarks for Casita and overlapping tools.

The runner intentionally uses only the Python standard library.  Every timed
sample gets a fresh workspace; setup and validation happen outside the timed
region.  Raw samples are the source of truth and the Markdown report is derived
from them.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import math
import os
import pathlib
import platform
import random
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from typing import Iterable, Sequence

from benchmarks.lib.html_report import render_html


SCHEMA_VERSION = 1
SOURCE_DATE_EPOCH = 1_700_000_000
OPERATIONS = (
    "cold-import",
    "unchanged-import",
    "edited-import", "edited-import-in-place",
    "checkout",
    "sync-cold",
    "sync-warm",
    "verify",
    "collect",
)
IMPLEMENTATIONS = ("casita", "git", "tar-zstd", "restic", "borg")
CORPORA = ("small-files", "mixed", "large-files")
CACHE_POLICIES = ("warm", "cold")
PACK_TRAILER_BYTES = 16
NIXPKGS_OPERATIONS = (
    "cold-import",
    "unchanged-import",
    "checkout",
    "verify",
)


@dataclasses.dataclass(frozen=True)
class CorpusScale:
    small_count: int
    small_size: int
    large_count: int
    large_size: int


SCALES = {
    "smoke": {
        "small-files": CorpusScale(24, 1024, 0, 0),
        "mixed": CorpusScale(12, 2048, 2, 128 * 1024),
        "large-files": CorpusScale(0, 0, 2, 256 * 1024),
    },
    "standard": {
        "small-files": CorpusScale(4096, 2048, 0, 0),
        "mixed": CorpusScale(512, 16 * 1024, 8, 4 * 1024 * 1024),
        "large-files": CorpusScale(0, 0, 4, 32 * 1024 * 1024),
    },
    # Large enough to fill candidate chunk packs and expose footer/index costs.
    # This is a tuning workload, not part of the comparator publication suite.
    "pack-tuning": {
        "small-files": CorpusScale(32768, 2048, 0, 0),
        "mixed": CorpusScale(2048, 16 * 1024, 12, 16 * 1024 * 1024),
        "large-files": CorpusScale(0, 0, 8, 64 * 1024 * 1024),
    },
}


@dataclasses.dataclass
class Corpus:
    name: str
    base: pathlib.Path
    edited: pathlib.Path
    base_manifest: dict[str, dict[str, object]]
    edited_manifest: dict[str, dict[str, object]]

    @property
    def base_bytes(self) -> int:
        return sum(
            int(item.get("size", 0))
            for item in self.base_manifest.values()
            if item["type"] == "file"
        )

    @property
    def edited_bytes(self) -> int:
        return sum(
            int(item.get("size", 0))
            for item in self.edited_manifest.values()
            if item["type"] == "file"
        )


@dataclasses.dataclass
class CommandSpec:
    steps: list[list[str]]
    cwd: pathlib.Path
    env: dict[str, str]

    def display(self) -> str:
        return " && ".join(shlex.join(step) for step in self.steps)


@dataclasses.dataclass
class Prepared:
    command: CommandSpec
    repository_paths: list[pathlib.Path]
    cache_paths: list[pathlib.Path]
    expected_manifest: dict[str, dict[str, object]] | None = None
    restored_path: pathlib.Path | None = None


class BenchmarkError(RuntimeError):
    pass


def run_checked(
    argv: Sequence[str],
    *,
    cwd: pathlib.Path | None = None,
    env: dict[str, str] | None = None,
) -> str:
    completed = subprocess.run(
        list(argv),
        cwd=cwd,
        env=env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        raise BenchmarkError(
            f"command failed ({completed.returncode}): {shlex.join(argv)}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed.stdout


def deterministic_bytes(seed: str, length: int) -> bytes:
    output = bytearray()
    counter = 0
    while len(output) < length:
        output.extend(hashlib.sha256(f"{seed}:{counter}".encode()).digest())
        counter += 1
    return bytes(output[:length])


def structured_bytes(seed: str, length: int) -> bytes:
    line = f"casita benchmark record {seed} = deterministic payload\n".encode()
    return (line * (length // len(line) + 1))[:length]


def set_fixed_metadata(root: pathlib.Path) -> None:
    for path in sorted(root.rglob("*"), reverse=True):
        if not path.is_symlink():
            os.utime(path, (SOURCE_DATE_EPOCH, SOURCE_DATE_EPOCH), follow_symlinks=False)
    os.utime(root, (SOURCE_DATE_EPOCH, SOURCE_DATE_EPOCH), follow_symlinks=False)


def tree_manifest(root: pathlib.Path) -> dict[str, dict[str, object]]:
    manifest: dict[str, dict[str, object]] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            manifest[relative] = {"type": "symlink", "target": os.readlink(path)}
        elif stat.S_ISDIR(info.st_mode):
            manifest[relative] = {"type": "directory"}
        elif stat.S_ISREG(info.st_mode):
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            manifest[relative] = {
                "type": "file",
                "size": info.st_size,
                "sha256": digest,
                "executable": bool(info.st_mode & stat.S_IXUSR),
            }
        else:
            raise BenchmarkError(f"unsupported generated file type: {path}")
    return manifest


def generate_corpus(root: pathlib.Path, name: str, scale: CorpusScale) -> Corpus:
    base = root / name / "base" / "corpus"
    edited = root / name / "edited" / "corpus"
    base.mkdir(parents=True)

    for index in range(scale.small_count):
        group = base / f"group-{index % 32:02d}"
        group.mkdir(exist_ok=True)
        path = group / f"file-{index:06d}.dat"
        if index % 4:
            data = structured_bytes(f"{name}-small-{index}", scale.small_size + index % 97)
        else:
            data = deterministic_bytes(f"{name}-small-{index}", scale.small_size + index % 97)
        path.write_bytes(data)
        path.chmod(0o755 if index % 37 == 0 else 0o644)

    large_dir = base / "large"
    if scale.large_count:
        large_dir.mkdir(exist_ok=True)
    for index in range(scale.large_count):
        path = large_dir / f"payload-{index:03d}.bin"
        if index % 2:
            data = structured_bytes(f"{name}-large-{index}", scale.large_size)
        else:
            data = deterministic_bytes(f"{name}-large-{index}", scale.large_size)
        path.write_bytes(data)
        path.chmod(0o644)

    marker = base / "README.benchmark"
    marker.write_bytes(structured_bytes(f"{name}-marker", 4096))
    marker.chmod(0o644)
    if os.name != "nt":
        (base / "current").symlink_to("README.benchmark")
    set_fixed_metadata(base)

    shutil.copytree(base, edited, symlinks=True)
    edit_target = next(path for path in sorted(edited.rglob("*")) if path.is_file())
    original = edit_target.read_bytes()
    midpoint = len(original) // 2
    edit_target.write_bytes(
        original[:midpoint]
        + deterministic_bytes(f"{name}-edit", min(8192, max(128, len(original))))
        + original[midpoint:]
    )
    added = edited / "added-after-base.txt"
    added.write_bytes(structured_bytes(f"{name}-added", 8192))
    added.chmod(0o644)
    set_fixed_metadata(edited)

    return Corpus(name, base, edited, tree_manifest(base), tree_manifest(edited))


def prepare_sample(adapter: Adapter, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
    if operation != "edited-import-in-place":
        return adapter.prepare(operation, corpus, workspace)
    source = workspace / "in-place" / "corpus"
    shutil.copytree(corpus.base, source, symlinks=True)
    local = dataclasses.replace(corpus, base=source, edited=source)
    # Each adapter imports the base during setup and constructs its timed
    # command before we edit. Untouched files keep all stat-cache identities.
    prepared = adapter.prepare("edited-import", local, workspace)
    apply_corpus_delta(corpus, source)
    return prepared


def apply_corpus_delta(corpus: Corpus, source: pathlib.Path) -> None:
    for relative in sorted(corpus.base_manifest.keys() - corpus.edited_manifest.keys(), reverse=True):
        path = source / relative
        if path.is_dir() and not path.is_symlink():
            path.rmdir()
        else:
            path.unlink()
    for relative, entry in corpus.edited_manifest.items():
        if corpus.base_manifest.get(relative) == entry:
            continue
        path = source / relative
        original = corpus.edited / relative
        if entry["type"] == "directory":
            path.mkdir(parents=True, exist_ok=True)
        elif entry["type"] == "symlink":
            if path.exists() or path.is_symlink():
                path.unlink()
            path.symlink_to(entry["target"])
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, path)
    assert_manifest(source, corpus.edited_manifest)


def manifest_identity(manifest: dict[str, dict[str, object]]) -> str:
    return hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest()


def materialize_nixpkgs_corpus(
    source: pathlib.Path,
    revision: str,
    destination: pathlib.Path,
) -> tuple[Corpus, dict[str, object]]:
    """Export one committed nixpkgs tree without importing worktree residue."""
    if not source.is_dir():
        raise BenchmarkError(f"nixpkgs source is not a directory: {source}")
    git = shutil.which("git")
    tar = shutil.which("tar")
    if git is None or tar is None:
        raise BenchmarkError("the nixpkgs corpus requires git and tar on PATH")

    commit = run_checked(
        [git, "-C", str(source), "rev-parse", "--verify", f"{revision}^{{commit}}"]
    ).strip()
    tree = run_checked(
        [git, "-C", str(source), "rev-parse", "--verify", f"{commit}^{{tree}}"]
    ).strip()
    if len(commit) != 40 or len(tree) != 40:
        raise BenchmarkError("nixpkgs did not resolve to full SHA-1 commit and tree identities")

    destination.mkdir(parents=True, exist_ok=True)
    archive = subprocess.Popen(
        [git, "-C", str(source), "archive", "--format=tar", "--prefix=nixpkgs/", commit],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert archive.stdout is not None
    assert archive.stderr is not None
    extractor = subprocess.Popen(
        [tar, "-xf", "-", "-C", str(destination)],
        stdin=archive.stdout,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    archive.stdout.close()
    _, extract_stderr = extractor.communicate()
    archive_stderr = archive.stderr.read()
    archive.stderr.close()
    archive_status = archive.wait()
    if archive_status != 0 or extractor.returncode != 0:
        raise BenchmarkError(
            "could not materialize the committed nixpkgs tree\n"
            f"git archive ({archive_status}): {archive_stderr.decode(errors='replace')[-4000:]}\n"
            f"tar ({extractor.returncode}): {extract_stderr.decode(errors='replace')[-4000:]}"
        )

    base = destination / "nixpkgs"
    manifest = tree_manifest(base)
    corpus = Corpus("nixpkgs", base, base, manifest, manifest)
    types = {
        kind: sum(item["type"] == kind for item in manifest.values())
        for kind in ("file", "directory", "symlink")
    }
    metadata: dict[str, object] = {
        "kind": "committed-git-tree",
        "revision": commit,
        "tree": tree,
        "paths": len(manifest),
        "regular_files": types["file"],
        "directories": types["directory"],
        "symlinks": types["symlink"],
        "logical_file_bytes": corpus.base_bytes,
        "manifest_sha256": manifest_identity(manifest),
    }
    return corpus, metadata


def assert_manifest(root: pathlib.Path, expected: dict[str, dict[str, object]]) -> None:
    actual = tree_manifest(root)
    if actual == expected:
        return
    missing = sorted(set(expected) - set(actual))[:10]
    extra = sorted(set(actual) - set(expected))[:10]
    changed = sorted(
        path for path in set(expected) & set(actual) if expected[path] != actual[path]
    )[:10]
    raise BenchmarkError(
        f"restored tree differs: missing={missing}, extra={extra}, changed={changed}"
    )


def filesystem_usage(paths: Iterable[pathlib.Path]) -> dict[str, int]:
    apparent = allocated = files = 0
    seen: set[tuple[int, int]] = set()
    for root in paths:
        if not root.exists():
            continue
        candidates = [root] if not root.is_dir() else [root, *root.rglob("*")]
        for path in candidates:
            try:
                info = path.lstat()
            except FileNotFoundError:
                continue
            identity = (info.st_dev, info.st_ino)
            if identity in seen:
                continue
            seen.add(identity)
            files += 1
            apparent += info.st_size
            allocated += getattr(info, "st_blocks", 0) * 512
    return {"apparent_bytes": apparent, "allocated_bytes": allocated, "entries": files}


def data_files(paths: Iterable[pathlib.Path]) -> Iterable[pathlib.Path]:
    for root in paths:
        if not root.exists():
            continue
        if root.is_file() and not root.is_symlink():
            yield root
        elif root.is_dir():
            for path in root.rglob("*"):
                if path.is_file() and not path.is_symlink():
                    yield path


def apply_cache_policy(policy: str, paths: Iterable[pathlib.Path]) -> str:
    files = list(data_files(paths))
    if policy == "warm":
        for path in files:
            with path.open("rb", buffering=0) as handle:
                while handle.read(1024 * 1024):
                    pass
        return "read every regular file before timing"
    if not hasattr(os, "posix_fadvise") or not hasattr(os, "POSIX_FADV_DONTNEED"):
        raise BenchmarkError("cold cache policy requires os.posix_fadvise(POSIX_FADV_DONTNEED)")
    if hasattr(os, "sync"):
        os.sync()
    for path in files:
        try:
            descriptor = os.open(path, os.O_RDONLY)
            try:
                os.posix_fadvise(descriptor, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(descriptor)
        except OSError as error:
            raise BenchmarkError(f"could not evict {path}: {error}") from error
    return "POSIX_FADV_DONTNEED for every regular file before timing"


def measured_command(
    spec: CommandSpec,
    stdout_path: pathlib.Path,
    stderr_path: pathlib.Path,
    *,
    check: bool = True,
) -> dict[str, object]:
    if not hasattr(os, "posix_spawn") or not hasattr(os, "wait4"):
        raise BenchmarkError("the benchmark timer requires POSIX posix_spawn/wait4")
    # Even posix_spawn inherits Python's RSS high-water mark on Linux. GNU
    # time forks after exec from a small native process and reports its child.
    timer = os.environ.get("CASITA_BENCH_TIME") or shutil.which("time")
    if timer is None:
        raise BenchmarkError("GNU time is required; enter the pinned devenv shell")
    shell_command = "cd -- " + shlex.quote(str(spec.cwd.resolve())) + " && " + " && ".join(
        shlex.join(step) for step in spec.steps
    )
    actions = [
        (os.POSIX_SPAWN_OPEN, descriptor, str(path.resolve()),
         os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        for descriptor, path in ((1, stdout_path), (2, stderr_path))
    ]
    started = time.perf_counter_ns()
    with tempfile.NamedTemporaryFile(prefix="casita-time-", mode="r+") as usage_file:
        invocation = [timer, "--quiet", "--format=%U %S %M", "--output=" + usage_file.name,
                      "--", "/bin/sh", "-c", shell_command]
        pid = os.posix_spawn(timer, invocation, spec.env, file_actions=actions)
        _, status, _ = os.wait4(pid, 0)
        fields = usage_file.read().split()
        if len(fields) != 3:
            raise BenchmarkError("GNU time did not report child resource usage: " + stderr_path.read_text(errors="replace")[-1000:])
        user_seconds, system_seconds = map(float, fields[:2])
        max_rss = int(fields[2]) * 1024
    elapsed = (time.perf_counter_ns() - started) / 1_000_000_000
    exit_code = os.waitstatus_to_exitcode(status)
    result: dict[str, object] = {
        "wall_seconds": elapsed,
        "user_seconds": user_seconds,
        "system_seconds": system_seconds,
        "max_rss_bytes": max_rss,
        "exit_code": exit_code,
    }
    if check and exit_code != 0:
        stdout = stdout_path.read_text(errors="replace")[-4000:]
        stderr = stderr_path.read_text(errors="replace")[-4000:]
        raise BenchmarkError(
            f"timed command failed ({exit_code}): {spec.display()}\n"
            f"stdout tail:\n{stdout}\nstderr tail:\n{stderr}"
        )
    return result


class Adapter:
    name = "adapter"
    supported_operations: tuple[str, ...] = ()

    def __init__(self, executable: str):
        self.executable = executable

    def version(self) -> str:
        return run_checked([self.executable, "--version"]).splitlines()[0]

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        raise NotImplementedError

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        if prepared.restored_path is not None and prepared.expected_manifest is not None:
            assert_manifest(prepared.restored_path, prepared.expected_manifest)

    def operation_metrics(self, operation: str, stdout: str) -> dict[str, int]:
        return {}

    def storage_metrics(self, repositories: Sequence[pathlib.Path]) -> dict[str, int]:
        return {}


class CasitaAdapter(Adapter):
    name = "casita"
    supported_operations = OPERATIONS

    def __init__(
        self,
        executable: str,
        pack_target_bytes: int | None = None,
        fsck_mode: str = "audit-only",
        post_validate_fsck: bool = True,
        incremental_sync: bool = False,
    ):
        super().__init__(executable)
        self.pack_target_bytes = pack_target_bytes
        self.fsck_mode = fsck_mode
        self.post_validate_fsck = post_validate_fsck
        self.incremental_sync = incremental_sync

    def fsck_command(self, repository: pathlib.Path) -> list[str]:
        return self.command(repository, "fsck", f"--{self.fsck_mode}")

    def env(self) -> dict[str, str]:
        return {
            **os.environ,
            "TZ": "UTC",
            "LC_ALL": "C",
            "CASITA_PACK_STATS": "1",
        }

    def cli_command(self, *args: str) -> list[str]:
        tuning = (
            ["--pack-target-bytes", str(self.pack_target_bytes)]
            if self.pack_target_bytes is not None
            else []
        )
        discovery = ["--incremental"] if args[:1] == ("sync",) and self.incremental_sync else []
        return [self.executable, *tuning, *args, *discovery]

    def command(self, repository: pathlib.Path, *args: str) -> list[str]:
        return self.cli_command("--repository", str(repository), *args)

    def storage_metrics(self, repositories: Sequence[pathlib.Path]) -> dict[str, int]:
        packs: list[pathlib.Path] = []
        loose_chunks = 0
        blob_roots: list[pathlib.Path] = []
        metadata_roots: list[pathlib.Path] = []
        for repository in repositories:
            blob_roots.append(repository / "blobs")
            if repository.exists():
                metadata_roots.extend(
                    path for path in repository.iterdir() if path.name != "blobs"
                )
            pack_root = repository / "blobs" / "packs" / "b3"
            if pack_root.exists():
                packs.extend(path for path in pack_root.rglob("*") if path.is_file())
            loose_root = repository / "blobs" / "chunks" / "b3"
            if loose_root.exists():
                loose_chunks += sum(path.is_file() for path in loose_root.rglob("*"))

        pack_bytes = 0
        footer_bytes = 0
        pack_entries = 0
        footer_tail_misses = 0
        largest_pack = 0
        tail_projections = {
            tail: {"gets": 0, "bytes": 0}
            for tail in (64 * 1024, 256 * 1024, 1024 * 1024, 4 * 1024 * 1024)
        }
        for path in packs:
            size = path.stat().st_size
            pack_bytes += size
            largest_pack = max(largest_pack, size)
            if size < 16:
                raise BenchmarkError(f"truncated Casita pack in benchmark repository: {path}")
            with path.open("rb") as handle:
                handle.seek(-16, os.SEEK_END)
                trailer = handle.read(16)
                if trailer[8:] != b"casitac1":
                    raise BenchmarkError(f"invalid Casita pack trailer in benchmark repository: {path}")
                footer_len = int.from_bytes(trailer[:8], "little")
                if footer_len < 8 or footer_len + 16 > size:
                    raise BenchmarkError(f"invalid Casita pack footer length in benchmark repository: {path}")
                handle.seek(-(16 + footer_len), os.SEEK_END)
                entries = int.from_bytes(handle.read(8), "little")
            footer_bytes += footer_len
            pack_entries += entries
            if footer_len + 16 > min(size, 1024 * 1024):
                footer_tail_misses += 1
            for tail, projection in tail_projections.items():
                first_read = min(size, tail)
                projection["gets"] += 1
                projection["bytes"] += first_read
                if footer_len + 16 > first_read:
                    projection["gets"] += 1
                    projection["bytes"] += footer_len

        blob_usage = filesystem_usage(blob_roots)
        metadata_usage = filesystem_usage(metadata_roots)
        metrics = {
            "pack_count": len(packs),
            "pack_bytes": pack_bytes,
            "pack_body_bytes": pack_bytes - footer_bytes - 16 * len(packs),
            "pack_footer_bytes": footer_bytes,
            "pack_entries": pack_entries,
            "largest_pack_bytes": largest_pack,
            "footer_tail_miss_packs": footer_tail_misses,
            "rebuild_gets_exact": 2 * len(packs),
            "rebuild_bytes_exact": footer_bytes + PACK_TRAILER_BYTES * len(packs),
            "loose_chunk_count": loose_chunks,
            "blob_apparent_bytes": blob_usage["apparent_bytes"],
            "blob_allocated_bytes": blob_usage["allocated_bytes"],
            "blob_entries": blob_usage["entries"],
            "metadata_apparent_bytes": metadata_usage["apparent_bytes"],
            "metadata_allocated_bytes": metadata_usage["allocated_bytes"],
            "metadata_entries": metadata_usage["entries"],
        }
        for tail, projection in tail_projections.items():
            label = f"{tail // 1024}k"
            metrics[f"rebuild_gets_tail_{label}"] = projection["gets"]
            metrics[f"rebuild_bytes_tail_{label}"] = projection["bytes"]
        return metrics

    def init(self, repository: pathlib.Path) -> None:
        run_checked(self.command(repository, "init"), env=self.env())

    def import_tree(self, repository: pathlib.Path, source: pathlib.Path) -> None:
        run_checked(
            self.command(repository, "import", str(source), "--root", "bench/current"),
            env=self.env(),
        )

    def root_key(self, repository: pathlib.Path) -> str:
        output = run_checked(
            self.command(repository, "root", "ls", "bench/current"), env=self.env()
        )
        line = next((line for line in output.splitlines() if line.endswith("  bench/current")), None)
        if line is None:
            raise BenchmarkError(f"Casita root output did not contain bench/current: {output}")
        return line.split()[0]

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        env = self.env()
        repository = workspace / "repository"
        self.init(repository)
        expected = corpus.base_manifest
        restored: pathlib.Path | None = None
        repositories = [repository]

        if operation == "cold-import":
            steps = [self.command(repository, "import", str(corpus.base), "--root", "bench/current")]
        elif operation == "unchanged-import":
            self.import_tree(repository, corpus.base)
            steps = [self.command(repository, "import", str(corpus.base), "--root", "bench/current")]
        elif operation == "edited-import":
            self.import_tree(repository, corpus.base)
            expected = corpus.edited_manifest
            steps = [self.command(repository, "import", str(corpus.edited), "--root", "bench/current")]
        elif operation == "checkout":
            self.import_tree(repository, corpus.base)
            restored = workspace / "restored"
            steps = [self.command(repository, "checkout", self.root_key(repository), str(restored), "--no-root")]
        elif operation in ("sync-cold", "sync-warm"):
            source = workspace / "source-repository"
            destination = workspace / "destination-repository"
            repository = destination
            repositories = [source, destination]
            self.init(source)
            self.init(destination)
            self.import_tree(source, corpus.base)
            if operation == "sync-warm":
                run_checked(
                    self.cli_command("sync", "--from", str(source), "--to", str(destination), "--root", "bench/current"),
                    env=env,
                )
                self.import_tree(source, corpus.edited)
                expected = corpus.edited_manifest
            steps = [self.cli_command("sync", "--from", str(source), "--to", str(destination), "--root", "bench/current")]
        elif operation == "verify":
            self.import_tree(repository, corpus.base)
            steps = [self.fsck_command(repository)]
        elif operation == "collect":
            self.import_tree(repository, corpus.base)
            self.import_tree(repository, corpus.edited)
            expected = corpus.edited_manifest
            steps = [self.command(repository, "gc")]
        else:
            raise AssertionError(operation)

        return Prepared(
            CommandSpec(steps, workspace, env),
            repositories,
            [corpus.base, corpus.edited, *repositories],
            expected,
            restored,
        )

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        super().validate(operation, corpus, workspace, prepared)
        repository = prepared.repository_paths[-1]
        if self.post_validate_fsck:
            run_checked(self.fsck_command(repository), env=self.env())
        if operation != "checkout":
            restored = workspace / "validation"
            run_checked(
                self.command(repository, "checkout", self.root_key(repository), str(restored), "--no-root"),
                env=self.env(),
            )
            assert_manifest(restored, prepared.expected_manifest or {})

    def operation_metrics(self, operation: str, stdout: str) -> dict[str, int]:
        pack_names = {
            "pack-list-requests",
            "pack-footer-range-requests",
            "pack-footer-range-bytes",
            "pack-chunk-range-requests",
            "pack-chunk-range-bytes",
            "pack-whole-requests",
            "pack-whole-bytes",
            "pack-cache-hits",
            "pack-cache-promotions",
            "pack-cache-evictions",
            "pack-gc-replacement-put-requests",
            "pack-gc-replacement-put-bytes",
            "pack-gc-marker-put-requests",
            "pack-gc-marker-put-bytes",
            "pack-gc-delete-requests",
            "pack-gc-tombstone-put-requests",
            "pack-gc-tombstone-put-bytes",
            "pack-gc-deferred-packs",
            "pack-index-pointer-requests",
            "pack-index-requests",
            "pack-index-bytes",
            "pack-index-hash-nanos",
            "pack-index-decode-nanos",
            "pack-index-hits",
            "pack-index-fallbacks",
            "pack-index-put-requests",
            "pack-index-put-bytes",
        }
        names = {
            "published-objects",
            "payloads-sent",
            "payloads-reused",
            "chunks-sent",
            "chunks-reused",
            "physical-fsck-nanos",
            "logical-fsck-nanos",
            *pack_names,
            *(f"source-{name}" for name in pack_names),
        }
        metrics: dict[str, int] = {}
        for line in stdout.splitlines():
            parts = line.split()
            if len(parts) == 2 and parts[0] in names:
                try:
                    metrics[parts[0].replace("-", "_")] = int(parts[1])
                except ValueError:
                    continue
        return metrics


class GitAdapter(Adapter):
    name = "git"
    supported_operations = OPERATIONS

    def __init__(self, executable: str, pack_after_ingest: bool = False):
        super().__init__(executable)
        self.pack_after_ingest = pack_after_ingest

    def env(self) -> dict[str, str]:
        return {
            **os.environ,
            "TZ": "UTC",
            "LC_ALL": "C",
            "GIT_AUTHOR_NAME": "Casita Benchmark",
            "GIT_AUTHOR_EMAIL": "benchmark@invalid",
            "GIT_COMMITTER_NAME": "Casita Benchmark",
            "GIT_COMMITTER_EMAIL": "benchmark@invalid",
            "GIT_AUTHOR_DATE": f"@{SOURCE_DATE_EPOCH} +0000",
            "GIT_COMMITTER_DATE": f"@{SOURCE_DATE_EPOCH} +0000",
        }

    def init(self, repository: pathlib.Path, *, bare: bool = False) -> None:
        args = [self.executable, "init", "--quiet"]
        if bare:
            args.append("--bare")
        else:
            args.extend(["--initial-branch", "main"])
        args.append(str(repository))
        run_checked(args, env=self.env())
        run_checked([self.executable, "-C", str(repository), "config", "gc.auto", "0"], env=self.env())
        run_checked([self.executable, "-C", str(repository), "config", "gc.autoDetach", "false"], env=self.env())
        run_checked([self.executable, "-C", str(repository), "config", "maintenance.auto", "false"], env=self.env())
        run_checked([self.executable, "-C", str(repository), "config", "maintenance.autoDetach", "false"], env=self.env())
        run_checked(
            [self.executable, "-C", str(repository), "config", "core.logAllRefUpdates", "false"], env=self.env()
        )

    def ingest_steps(self, repository: pathlib.Path, source: pathlib.Path, message: str) -> list[list[str]]:
        git_dir = repository / ".git"
        prefix = [self.executable, f"--git-dir={git_dir}", f"--work-tree={source}"]
        # This adapter compares filesystem snapshots, not Git's ignore policy.
        # A committed source tree may legitimately contain paths matched by
        # its own .gitignore, so the fresh comparator repository must include
        # every exported path exactly as Casita does.
        steps = [
            [*prefix, "add", "--all", "--force"],
            [*prefix, "commit", "--quiet", "--allow-empty", "-m", message],
        ]
        if self.pack_after_ingest:
            steps.append(
                [
                    self.executable,
                    f"--git-dir={git_dir}",
                    "gc",
                    "--quiet",
                    "--prune=now",
                ]
            )
        return steps

    def ingest(self, repository: pathlib.Path, source: pathlib.Path, message: str) -> None:
        for step in self.ingest_steps(repository, source, message):
            run_checked(step, env=self.env())

    def checkout(self, repository: pathlib.Path, destination: pathlib.Path) -> None:
        destination.mkdir(parents=True)
        run_checked(
            [
                self.executable,
                f"--git-dir={repository / '.git'}",
                f"--work-tree={destination}",
                "checkout-index",
                "--all",
            ],
            env=self.env(),
        )

    def storage_metrics(self, repositories: Sequence[pathlib.Path]) -> dict[str, int]:
        pack_files: list[pathlib.Path] = []
        index_files: list[pathlib.Path] = []
        loose_objects = 0
        for repository in repositories:
            git_dir = repository if repository.name.endswith(".git") else repository / ".git"
            object_root = git_dir / "objects"
            pack_root = object_root / "pack"
            if pack_root.exists():
                pack_files.extend(pack_root.glob("*.pack"))
                index_files.extend(pack_root.glob("*.idx"))
            for prefix in object_root.iterdir() if object_root.exists() else ():
                if len(prefix.name) == 2 and prefix.is_dir():
                    loose_objects += sum(path.is_file() for path in prefix.iterdir())
        return {
            "pack_count": len(pack_files),
            "pack_bytes": sum(path.stat().st_size for path in pack_files),
            "index_bytes": sum(path.stat().st_size for path in index_files),
            "loose_object_count": loose_objects,
        }

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        env = self.env()
        repository = workspace / "repository"
        self.init(repository)
        expected = corpus.base_manifest
        restored: pathlib.Path | None = None
        repositories = [repository / ".git"]

        if operation == "cold-import":
            steps = self.ingest_steps(repository, corpus.base, "base")
        elif operation == "unchanged-import":
            self.ingest(repository, corpus.base, "base")
            steps = self.ingest_steps(repository, corpus.base, "unchanged")
        elif operation == "edited-import":
            self.ingest(repository, corpus.base, "base")
            expected = corpus.edited_manifest
            steps = self.ingest_steps(repository, corpus.edited, "edited")
        elif operation == "checkout":
            self.ingest(repository, corpus.base, "base")
            restored = workspace / "restored"
            restored.mkdir()
            steps = [[self.executable, f"--git-dir={repository / '.git'}", f"--work-tree={restored}", "checkout-index", "--all"]]
        elif operation in ("sync-cold", "sync-warm"):
            source = workspace / "source"
            destination = workspace / "destination.git"
            self.init(source)
            self.init(destination, bare=True)
            self.ingest(source, corpus.base, "base")
            if operation == "sync-warm":
                run_checked(
                    [self.executable, f"--git-dir={destination}", "fetch", "--quiet", str(source / '.git'), "refs/heads/main:refs/heads/main"],
                    env=env,
                )
                self.ingest(source, corpus.edited, "edited")
                expected = corpus.edited_manifest
            repository = destination
            repositories = [source / ".git", destination]
            steps = [[self.executable, f"--git-dir={destination}", "fetch", "--quiet", str(source / '.git'), "refs/heads/main:refs/heads/main"]]
        elif operation == "verify":
            self.ingest(repository, corpus.base, "base")
            steps = [[self.executable, f"--git-dir={repository / '.git'}", "fsck", "--full", "--strict", "--no-progress", "--no-dangling"]]
        elif operation == "collect":
            self.ingest(repository, corpus.base, "base")
            run_checked([self.executable, f"--git-dir={repository / '.git'}", "update-ref", "-d", "refs/heads/main"], env=env)
            run_checked([self.executable, f"--git-dir={repository / '.git'}", "read-tree", "--empty"], env=env)
            self.ingest(repository, corpus.edited, "edited")
            expected = corpus.edited_manifest
            steps = [[self.executable, f"--git-dir={repository / '.git'}", "gc", "--quiet", "--prune=now"]]
        else:
            raise AssertionError(operation)

        return Prepared(CommandSpec(steps, workspace, env), repositories, [corpus.base, corpus.edited, *repositories], expected, restored)

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        super().validate(operation, corpus, workspace, prepared)
        repository = prepared.repository_paths[-1]
        git_dir = repository if repository.name.endswith(".git") else repository / ".git"
        run_checked([self.executable, f"--git-dir={git_dir}", "fsck", "--full", "--strict", "--no-progress"], env=self.env())
        if operation != "checkout":
            restored = workspace / "validation"
            restored.mkdir()
            run_checked(
                [self.executable, f"--git-dir={git_dir}", f"--work-tree={restored}", "read-tree", "main"],
                env=self.env(),
            )
            run_checked(
                [self.executable, f"--git-dir={git_dir}", f"--work-tree={restored}", "checkout-index", "--all"],
                env=self.env(),
            )
            assert_manifest(restored, prepared.expected_manifest or {})


class TarZstdAdapter(Adapter):
    name = "tar-zstd"
    supported_operations = ("cold-import", "unchanged-import", "edited-import", "edited-import-in-place", "checkout", "verify")

    def __init__(self, tar: str, zstd: str):
        super().__init__(tar)
        self.tar = tar
        self.zstd = zstd

    def version(self) -> str:
        tar_version = run_checked([self.tar, "--version"]).splitlines()[0]
        zstd_version = run_checked([self.zstd, "--version"]).splitlines()[0]
        if "GNU tar" not in tar_version:
            raise BenchmarkError("tar-zstd adapter requires GNU tar for deterministic flags")
        return f"{tar_version}; {zstd_version}"

    def archive_step(self, source: pathlib.Path, archive: pathlib.Path) -> list[str]:
        tar = shlex.join(
            [self.tar, "--sort=name", f"--mtime=@{SOURCE_DATE_EPOCH}", "--owner=0", "--group=0", "--numeric-owner", "-C", str(source.parent), "-cf", "-", source.name]
        )
        zstd = shlex.join([self.zstd, "--quiet", "-T1", "-3", "--force", "-o", str(archive)])
        return ["/bin/sh", "-c", f"{tar} | {zstd}"]

    def create_archive(self, source: pathlib.Path, archive: pathlib.Path) -> None:
        run_checked(self.archive_step(source, archive))

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        env = {**os.environ, "TZ": "UTC", "LC_ALL": "C"}
        archive = workspace / "corpus.tar.zst"
        expected = corpus.base_manifest
        restored: pathlib.Path | None = None
        if operation == "cold-import":
            steps = [self.archive_step(corpus.base, archive)]
        elif operation == "unchanged-import":
            self.create_archive(corpus.base, archive)
            steps = [self.archive_step(corpus.base, archive)]
        elif operation == "edited-import":
            self.create_archive(corpus.base, archive)
            expected = corpus.edited_manifest
            steps = [self.archive_step(corpus.edited, archive)]
        elif operation == "checkout":
            self.create_archive(corpus.base, archive)
            destination = workspace / "restored-parent"
            destination.mkdir()
            restored = destination / "corpus"
            pipeline = f"{shlex.join([self.zstd, '--quiet', '-dc', str(archive)])} | {shlex.join([self.tar, '-xf', '-', '-C', str(destination)])}"
            steps = [["/bin/sh", "-c", pipeline]]
        elif operation == "verify":
            self.create_archive(corpus.base, archive)
            pipeline = f"{shlex.join([self.zstd, '--quiet', '-dc', str(archive)])} | {shlex.join([self.tar, '-tf', '-'])} >/dev/null"
            steps = [["/bin/sh", "-c", pipeline]]
        else:
            raise AssertionError(operation)
        return Prepared(CommandSpec(steps, workspace, env), [archive], [corpus.base, corpus.edited, archive], expected, restored)

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        super().validate(operation, corpus, workspace, prepared)
        archive = prepared.repository_paths[0]
        run_checked([self.zstd, "--quiet", "--test", str(archive)])
        if operation != "checkout":
            destination = workspace / "validation"
            destination.mkdir()
            pipeline = (
                f"{shlex.join([self.zstd, '--quiet', '-dc', str(archive)])} | "
                f"{shlex.join([self.tar, '-xf', '-', '-C', str(destination)])}"
            )
            run_checked(["/bin/sh", "-c", pipeline])
            assert_manifest(destination / "corpus", prepared.expected_manifest or {})


class ResticAdapter(Adapter):
    name = "restic"
    supported_operations = OPERATIONS

    def version(self) -> str:
        return run_checked([self.executable, "version"]).splitlines()[0]

    def env(self, repository: pathlib.Path, cache: pathlib.Path, password: pathlib.Path) -> dict[str, str]:
        return {
            **os.environ,
            "TZ": "UTC",
            "LC_ALL": "C",
            "RESTIC_REPOSITORY": str(repository),
            "RESTIC_PASSWORD_FILE": str(password),
            "RESTIC_CACHE_DIR": str(cache),
        }

    def init(self, repository: pathlib.Path, cache: pathlib.Path, password: pathlib.Path) -> None:
        run_checked([self.executable, "init"], env=self.env(repository, cache, password))

    def backup_step(self, source: pathlib.Path, timestamp: int) -> list[str]:
        fixed_time = dt.datetime.fromtimestamp(timestamp, dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
        return [self.executable, "backup", "--quiet", "--host", "benchmark", "--tag", "casita-benchmark", "--time", fixed_time, source.name]

    def backup(self, source: pathlib.Path, env: dict[str, str], timestamp: int) -> None:
        run_checked(self.backup_step(source, timestamp), cwd=source.parent, env=env)

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        repository = workspace / "repository"
        cache = workspace / "cache"
        password = workspace / "password"
        password.write_text("casita-repeatable-benchmark\n")
        env = self.env(repository, cache, password)
        self.init(repository, cache, password)
        expected = corpus.base_manifest
        restored: pathlib.Path | None = None
        repositories = [repository, cache]
        cwd = corpus.base.parent

        if operation == "cold-import":
            steps = [self.backup_step(corpus.base, SOURCE_DATE_EPOCH)]
        elif operation == "unchanged-import":
            self.backup(corpus.base, env, SOURCE_DATE_EPOCH)
            steps = [self.backup_step(corpus.base, SOURCE_DATE_EPOCH + 1)]
        elif operation == "edited-import":
            self.backup(corpus.base, env, SOURCE_DATE_EPOCH)
            expected = corpus.edited_manifest
            cwd = corpus.edited.parent
            steps = [self.backup_step(corpus.edited, SOURCE_DATE_EPOCH + 1)]
        elif operation == "checkout":
            self.backup(corpus.base, env, SOURCE_DATE_EPOCH)
            destination = workspace / "restored-parent"
            restored = destination / "corpus"
            steps = [[self.executable, "restore", "latest", "--quiet", "--target", str(destination)]]
        elif operation in ("sync-cold", "sync-warm"):
            source_repository = workspace / "source-repository"
            source_cache = workspace / "source-cache"
            destination_repository = workspace / "destination-repository"
            destination_cache = workspace / "destination-cache"
            self.init(source_repository, source_cache, password)
            self.init(destination_repository, destination_cache, password)
            source_env = self.env(source_repository, source_cache, password)
            self.backup(corpus.base, source_env, SOURCE_DATE_EPOCH)
            env = self.env(destination_repository, destination_cache, password)
            copy = [self.executable, "copy", "--quiet", "--from-repo", str(source_repository), "--from-password-file", str(password), "latest"]
            if operation == "sync-warm":
                run_checked(copy, env=env)
                self.backup(corpus.edited, source_env, SOURCE_DATE_EPOCH + 1)
                expected = corpus.edited_manifest
            steps = [copy]
            repository = destination_repository
            repositories = [source_repository, source_cache, destination_repository, destination_cache]
            cwd = workspace
        elif operation == "verify":
            self.backup(corpus.base, env, SOURCE_DATE_EPOCH)
            steps = [[self.executable, "check", "--quiet", "--read-data"]]
        elif operation == "collect":
            self.backup(corpus.base, env, SOURCE_DATE_EPOCH)
            self.backup(corpus.edited, env, SOURCE_DATE_EPOCH + 1)
            expected = corpus.edited_manifest
            steps = [[self.executable, "forget", "--quiet", "--host", "benchmark", "--tag", "casita-benchmark", "--keep-last", "1", "--prune"]]
        else:
            raise AssertionError(operation)
        return Prepared(CommandSpec(steps, cwd, env), repositories, [corpus.base, corpus.edited, *repositories], expected, restored)

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        super().validate(operation, corpus, workspace, prepared)
        repository = prepared.repository_paths[-2] if prepared.repository_paths[-1].name.endswith("cache") else prepared.repository_paths[-1]
        cache = prepared.repository_paths[-1] if prepared.repository_paths[-1].name.endswith("cache") else workspace / "cache"
        password = workspace / "password"
        env = self.env(repository, cache, password)
        run_checked([self.executable, "check", "--quiet", "--read-data"], env=env)
        if operation != "checkout":
            destination = workspace / "validation"
            run_checked([self.executable, "restore", "latest", "--quiet", "--target", str(destination)], env=env)
            assert_manifest(destination / "corpus", prepared.expected_manifest or {})


class BorgAdapter(Adapter):
    name = "borg"
    supported_operations = ("cold-import", "unchanged-import", "edited-import", "edited-import-in-place", "checkout", "verify", "collect")

    def version(self) -> str:
        version = super().version()
        if not version.split()[-1].startswith("1."):
            raise BenchmarkError("the Borg adapter currently supports Borg 1.x command syntax")
        return version

    def env(self, cache: pathlib.Path) -> dict[str, str]:
        return {
            **os.environ,
            "TZ": "UTC",
            "LC_ALL": "C",
            "BORG_CACHE_DIR": str(cache),
            "BORG_UNKNOWN_UNENCRYPTED_REPO_ACCESS_IS_OK": "yes",
            "BORG_RELOCATED_REPO_ACCESS_IS_OK": "yes",
        }

    def init(self, repository: pathlib.Path, env: dict[str, str]) -> None:
        run_checked([self.executable, "init", "--encryption=none", str(repository)], env=env)

    def create_step(self, repository: pathlib.Path, archive: str, source: pathlib.Path) -> list[str]:
        return [self.executable, "create", "--compression", "zstd,3", f"{repository}::{archive}", source.name]

    def create(self, repository: pathlib.Path, archive: str, source: pathlib.Path, env: dict[str, str]) -> None:
        run_checked(self.create_step(repository, archive, source), cwd=source.parent, env=env)

    def prepare(self, operation: str, corpus: Corpus, workspace: pathlib.Path) -> Prepared:
        repository = workspace / "repository"
        cache = workspace / "cache"
        env = self.env(cache)
        self.init(repository, env)
        expected = corpus.base_manifest
        restored: pathlib.Path | None = None
        cwd = corpus.base.parent
        if operation == "cold-import":
            steps = [self.create_step(repository, "base", corpus.base)]
        elif operation == "unchanged-import":
            self.create(repository, "base", corpus.base, env)
            steps = [self.create_step(repository, "unchanged", corpus.base)]
        elif operation == "edited-import":
            self.create(repository, "base", corpus.base, env)
            expected = corpus.edited_manifest
            cwd = corpus.edited.parent
            steps = [self.create_step(repository, "edited", corpus.edited)]
        elif operation == "checkout":
            self.create(repository, "base", corpus.base, env)
            destination = workspace / "restored-parent"
            destination.mkdir()
            restored = destination / "corpus"
            cwd = destination
            steps = [[self.executable, "extract", f"{repository}::base"]]
        elif operation == "verify":
            self.create(repository, "base", corpus.base, env)
            steps = [[self.executable, "check", "--verify-data", str(repository)]]
            cwd = workspace
        elif operation == "collect":
            self.create(repository, "base", corpus.base, env)
            self.create(repository, "edited", corpus.edited, env)
            expected = corpus.edited_manifest
            steps = [[self.executable, "delete", f"{repository}::base"], [self.executable, "compact", str(repository)]]
            cwd = workspace
        else:
            raise AssertionError(operation)
        return Prepared(CommandSpec(steps, cwd, env), [repository, cache], [corpus.base, corpus.edited, repository, cache], expected, restored)

    def validate(self, operation: str, corpus: Corpus, workspace: pathlib.Path, prepared: Prepared) -> None:
        super().validate(operation, corpus, workspace, prepared)
        repository, cache = prepared.repository_paths
        env = self.env(cache)
        run_checked([self.executable, "check", "--verify-data", str(repository)], env=env)
        if operation != "checkout":
            archive = "edited" if operation in ("edited-import", "edited-import-in-place", "collect") else "unchanged" if operation == "unchanged-import" else "base"
            destination = workspace / "validation"
            destination.mkdir()
            run_checked([self.executable, "extract", f"{repository}::{archive}"], cwd=destination, env=env)
            assert_manifest(destination / "corpus", prepared.expected_manifest or {})


def executable(name: str) -> str | None:
    return shutil.which(name)


def make_adapters(args: argparse.Namespace) -> tuple[dict[str, Adapter], dict[str, str]]:
    selected = set(args.implementations)
    adapters: dict[str, Adapter] = {}
    skipped: dict[str, str] = {}
    candidates: dict[str, Adapter | None] = {
        "casita": CasitaAdapter(
            str(args.casita_bin),
            args.casita_pack_target_bytes,
            args.casita_fsck_mode,
            not args.skip_post_fsck,
            args.casita_incremental_sync,
        )
        if args.casita_bin.exists()
        else None,
        "git": GitAdapter(executable("git") or "", args.git_pack_comparator)
        if executable("git")
        else None,
        "tar-zstd": TarZstdAdapter(executable("tar") or "", executable("zstd") or "") if executable("tar") and executable("zstd") else None,
        "restic": ResticAdapter(executable("restic") or "") if executable("restic") else None,
        "borg": BorgAdapter(executable("borg") or "") if executable("borg") else None,
    }
    for name in args.implementations:
        adapter = candidates[name]
        if adapter is None:
            skipped[name] = "required executable is not on PATH"
            continue
        try:
            adapter.version()
        except (BenchmarkError, OSError) as error:
            skipped[name] = str(error)
            continue
        adapters[name] = adapter
    if args.require_all and skipped:
        raise BenchmarkError(f"required implementations unavailable: {skipped}")
    if not adapters:
        raise BenchmarkError(f"no selected implementations are available: {selected}; skipped={skipped}")
    return adapters, skipped


def tool_versions(adapters: dict[str, Adapter], skipped: dict[str, str]) -> dict[str, dict[str, str]]:
    result = {name: {"status": "available", "version": adapter.version()} for name, adapter in adapters.items()}
    result.update({name: {"status": "skipped", "reason": reason} for name, reason in skipped.items()})
    return dict(sorted(result.items()))


def command_version(argv: Sequence[str]) -> str | None:
    try:
        return run_checked(argv).splitlines()[0]
    except (BenchmarkError, OSError, IndexError):
        return None


def environment_metadata(work_root: pathlib.Path) -> dict[str, object]:
    cpu = platform.processor()
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(errors="replace").splitlines():
            if line.lower().startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    governor_path = pathlib.Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    governor = governor_path.read_text().strip() if governor_path.exists() else None
    filesystem = command_version(["findmnt", "--noheadings", "--output", "FSTYPE,SOURCE,OPTIONS", "--target", str(work_root)])
    revision = command_version(["git", "rev-parse", "HEAD"])
    dirty = bool(run_checked(["git", "status", "--porcelain"]).strip()) if executable("git") else None
    return {
        "captured_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "hostname": platform.node(),
        "platform": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu,
        "logical_cpus": os.cpu_count(),
        "memory_bytes": os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") if hasattr(os, "sysconf") else None,
        "cpu_governor": governor,
        "filesystem": filesystem,
        "python": platform.python_version(),
        "rustc": command_version(["rustc", "--version"]),
        "cargo": command_version(["cargo", "--version"]),
        "casita_revision": revision,
        "casita_worktree_dirty": dirty,
    }


def percentile(values: Sequence[float], quantile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("percentile of empty sequence")
    return ordered[max(0, math.ceil(quantile * len(ordered)) - 1)]


def aggregates(samples: Sequence[dict[str, object]]) -> list[dict[str, object]]:
    grouped: dict[tuple[str, str, str, str], list[dict[str, object]]] = defaultdict(list)
    for sample in samples:
        if sample["status"] == "ok":
            key = (
                str(sample["corpus"]),
                str(sample["cache_policy"]),
                str(sample["operation"]),
                str(sample["implementation"]),
            )
            grouped[key].append(sample)
    output = []
    for (corpus, cache, operation, implementation), group in sorted(grouped.items()):
        walls = [float(item["wall_seconds"]) for item in group]
        throughputs = [float(item["throughput_bytes_per_second"]) for item in group]
        rss = [float(item["max_rss_bytes"]) for item in group]
        allocated = [float(item["repository_usage"]["allocated_bytes"]) for item in group]  # type: ignore[index]
        storage_metric_names = sorted(
            {
                name
                for item in group
                for name, value in item.get("storage_metrics", {}).items()  # type: ignore[union-attr]
                if isinstance(value, (int, float)) and not isinstance(value, bool)
            }
        )
        storage_metrics = {
            name: percentile(
                [
                    float(item["storage_metrics"][name])  # type: ignore[index]
                    for item in group
                    if isinstance(item.get("storage_metrics", {}).get(name), (int, float))  # type: ignore[union-attr]
                ],
                0.5,
            )
            for name in storage_metric_names
        }
        operation_metric_names = sorted(
            {
                name
                for item in group
                for name, value in item.get("operation_metrics", {}).items()  # type: ignore[union-attr]
                if isinstance(value, (int, float)) and not isinstance(value, bool)
            }
        )
        operation_metrics = {
            name: percentile(
                [
                    float(item["operation_metrics"][name])  # type: ignore[index]
                    for item in group
                    if isinstance(item.get("operation_metrics", {}).get(name), (int, float))  # type: ignore[union-attr]
                ],
                0.5,
            )
            for name in operation_metric_names
        }
        output.append(
            {
                "corpus": corpus,
                "cache_policy": cache,
                "operation": operation,
                "implementation": implementation,
                "samples": len(group),
                "median_wall_seconds": percentile(walls, 0.5),
                "p95_wall_seconds": percentile(walls, 0.95),
                "median_throughput_bytes_per_second": percentile(throughputs, 0.5),
                "median_max_rss_bytes": percentile(rss, 0.5),
                "median_repository_allocated_bytes": percentile(allocated, 0.5),
                "median_storage_metrics": storage_metrics,
                "median_operation_metrics": operation_metrics,
            }
        )
    return output


def human_bytes(value: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(value) < 1024 or unit == "TiB":
            return f"{value:.1f} {unit}"
        value /= 1024
    raise AssertionError


def render_report(result: dict[str, object]) -> str:
    corpora = result.get("corpora", {})
    is_nixpkgs = isinstance(corpora, dict) and "nixpkgs" in corpora
    lines = [
        (
            "# Nixpkgs packed-backend benchmark report"
            if is_nixpkgs
            else "# Casita end-to-end benchmark report"
        ),
        "",
        f"Schema: `{result['schema_version']}`. Casita revision: `{result['environment'].get('casita_revision')}`.",  # type: ignore[union-attr]
        "",
        "Raw samples in the adjacent JSON file are authoritative. Times include the documented operation only; setup and validation are outside the timed region.",
        "",
    ]
    if is_nixpkgs:
        nixpkgs = corpora["nixpkgs"]  # type: ignore[index]
        lines.extend(
            [
                "The source is exported from the committed Git tree; dirty and untracked worktree files are excluded. The local checkout path is redacted from the recorded invocation.",
                "",
                f"Source revision: `{nixpkgs.get('revision')}`; tree: `{nixpkgs.get('tree')}`; paths: `{nixpkgs.get('paths')}`; logical file bytes: `{nixpkgs.get('logical_file_bytes')}`.",
                "",
            ]
        )
    lines.extend(
        [
            "| Corpus | Cache | Operation | Implementation | n | Median | p95 | Median throughput | Peak RSS | Repository allocation |",
            "|---|---|---|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for item in result["aggregates"]:  # type: ignore[union-attr]
        lines.append(
            "| {corpus} | {cache_policy} | {operation} | {implementation} | {samples} | {median_wall_seconds:.4f} s | {p95_wall_seconds:.4f} s | {throughput} | {rss} | {storage} |".format(
                **item,
                throughput=human_bytes(float(item["median_throughput_bytes_per_second"])) + "/s",
                rss=human_bytes(float(item["median_max_rss_bytes"])),
                storage=human_bytes(float(item["median_repository_allocated_bytes"])),
            )
        )
    if is_nixpkgs:
        lines.extend(
            [
                "",
                "## Storage shape",
                "",
                "Pack bytes are immutable pack objects only. Blob allocation includes packs, manifests, outboards, and catalog objects; metadata allocation is everything outside `blobs/`, principally logical state and ingest metadata.",
                "",
                "| Cache | Operation | Implementation | Repository allocation | Pack count | Pack bytes | Blob allocation | Metadata allocation | Loose objects |",
                "|---|---|---|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for item in result["aggregates"]:  # type: ignore[union-attr]
            storage = item.get("median_storage_metrics", {})

            def count(name: str) -> str:
                value = storage.get(name)
                return f"{int(value):,}" if isinstance(value, (int, float)) else "—"

            def byte_count(name: str) -> str:
                value = storage.get(name)
                return human_bytes(float(value)) if isinstance(value, (int, float)) else "—"

            loose_name = (
                "loose_chunk_count"
                if "loose_chunk_count" in storage
                else "loose_object_count"
            )
            lines.append(
                f"| {item['cache_policy']} | {item['operation']} | {item['implementation']} | "
                f"{human_bytes(float(item['median_repository_allocated_bytes']))} | "
                f"{count('pack_count')} | {byte_count('pack_bytes')} | "
                f"{byte_count('blob_allocated_bytes')} | {byte_count('metadata_allocated_bytes')} | "
                f"{count(loose_name)} |"
            )
    tools = result["tools"]  # type: ignore[assignment]
    skipped = [(name, info["reason"]) for name, info in tools.items() if info["status"] == "skipped"]
    if skipped:
        lines.extend(["", "## Skipped implementations", ""])
        lines.extend(f"- `{name}`: {reason}" for name, reason in skipped)
    failures = [sample for sample in result["samples"] if sample["status"] != "ok"]  # type: ignore[index]
    if failures:
        lines.extend(["", "## Failed samples", ""])
        lines.extend(
            f"- `{sample['implementation']} / {sample['corpus']} / {sample['operation']} / {sample['cache_policy']} / {sample['repetition']}`: {sample['error']}"
            for sample in failures
        )
    lines.extend(
        [
            "",
            "## Interpretation boundaries",
            "",
            "- Casita verifies namespace identity and graph closure before publication; comparator verification and publication guarantees differ.",
            "- Git is a source-tree object database and version-control system; restic and Borg are backup tools; tar+zstd is a one-shot archive baseline.",
            "- Restic's mandatory repository encryption is enabled. The Borg adapter uses `--encryption=none`, matching Casita's local plaintext-at-rest profile.",
            "- `warm` pre-reads regular files. `cold` uses `POSIX_FADV_DONTNEED`; neither controls filesystem metadata caches or storage-device caches.",
            "- Repository allocation includes tool indexes and metadata but excludes the source corpus and restored output.",
            "",
        ]
    )
    return "\n".join(lines)


def parse_csv(value: str, allowed: Sequence[str]) -> list[str]:
    values = [item.strip() for item in value.split(",") if item.strip()]
    unknown = sorted(set(values) - set(allowed))
    if unknown:
        raise argparse.ArgumentTypeError(f"unknown value(s): {', '.join(unknown)}")
    return values


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=sorted(SCALES), default="smoke")
    parser.add_argument("--corpora", default=None, help="comma-separated corpus names (default: all; smoke defaults to small-files)")
    parser.add_argument(
        "--nixpkgs",
        "--source",
        dest="nixpkgs",
        type=pathlib.Path,
        help="add a committed nixpkgs checkout as the real `nixpkgs` corpus",
    )
    parser.add_argument(
        "--nixpkgs-revision",
        default="HEAD",
        help="committed nixpkgs revision to export (default: HEAD)",
    )
    parser.add_argument("--operations", default=",".join(OPERATIONS), help="comma-separated operations")
    parser.add_argument("--implementations", default=",".join(IMPLEMENTATIONS), help="comma-separated implementations")
    parser.add_argument("--cache-policies", default="warm,cold", help="comma-separated warm,cold")
    parser.add_argument("--repetitions", type=int, default=None)
    parser.add_argument("--seed", type=int, default=73)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument(
        "--casita-revision",
        help="exact revision represented by an externally built --casita-bin",
    )
    parser.add_argument(
        "--casita-pack-target-bytes",
        type=int,
        help="override Casita's compressed pack target for tuning runs",
    )
    parser.add_argument(
        "--casita-fsck-mode",
        choices=("audit-only", "dry-run"),
        default="audit-only",
        help="Casita verification mode; dry-run supports older revisions",
    )
    parser.add_argument(
        "--skip-post-fsck",
        action="store_true",
        help="validate Casita results by checkout/manifest only; timed verify cases still run fsck",
    )
    parser.add_argument(
        "--git-pack-comparator",
        action="store_true",
        help="run git gc after each comparator ingest so storage and checkout use packs",
    )
    parser.add_argument("--no-build", action="store_true", help="do not build the Casita binary")
    parser.add_argument("--casita-incremental-sync", action="store_true", help="reuse verified destination closures; source descendants below them are not audited")
    parser.add_argument("--require-all", action="store_true", help="fail if any selected implementation is unavailable")
    parser.add_argument("--require-clean", action="store_true", help="fail unless the Casita Git worktree is clean")
    parser.add_argument("--keep-work", type=pathlib.Path, help="keep per-sample workspaces below this directory")
    parser.add_argument("--output", type=pathlib.Path, default=None)
    parser.add_argument("--report", type=pathlib.Path, default=None)
    parser.add_argument("--html", type=pathlib.Path, default=None, help="write a standalone interactive HTML report")
    parser.add_argument("--render-existing", type=pathlib.Path, help="validate raw JSON and regenerate its Markdown report")
    return parser


def normalize_args(args: argparse.Namespace) -> None:
    args.operations = parse_csv(args.operations, OPERATIONS)
    args.implementations = parse_csv(args.implementations, IMPLEMENTATIONS)
    args.cache_policies = parse_csv(args.cache_policies, CACHE_POLICIES)
    available_corpora = [*CORPORA, *(["nixpkgs"] if args.nixpkgs is not None else [])]
    default_corpora = (
        ["nixpkgs"]
        if args.nixpkgs is not None
        else (["small-files"] if args.profile == "smoke" else list(CORPORA))
    )
    args.corpora = (
        parse_csv(args.corpora, available_corpora) if args.corpora else default_corpora
    )
    if "nixpkgs" in args.corpora and args.nixpkgs is None:
        raise BenchmarkError("the nixpkgs corpus requires --source PATH")
    unsupported_nixpkgs = sorted(set(args.operations) - set(NIXPKGS_OPERATIONS))
    if "nixpkgs" in args.corpora and unsupported_nixpkgs:
        raise BenchmarkError(
            "the committed nixpkgs corpus does not define edited-state operations: "
            + ", ".join(unsupported_nixpkgs)
        )
    args.repetitions = args.repetitions if args.repetitions is not None else (1 if args.profile == "smoke" else 10)
    if args.repetitions < 1:
        raise BenchmarkError("repetitions must be positive")
    if args.casita_pack_target_bytes is not None and args.casita_pack_target_bytes < 1:
        raise BenchmarkError("--casita-pack-target-bytes must be positive")
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    args.output = args.output or pathlib.Path("benchmarks/results") / f"{timestamp}.json"
    args.report = args.report or args.output.with_suffix(".md")
    args.casita_bin = args.casita_bin.resolve()
    if args.nixpkgs is not None:
        args.nixpkgs = args.nixpkgs.resolve()


def redacted_argv(argv: Sequence[str]) -> list[str]:
    """Keep invocation shape without publishing a maintainer's source path."""
    redacted: list[str] = []
    hide_next = False
    for argument in argv:
        if hide_next:
            redacted.append("<local-nixpkgs-checkout>")
            hide_next = False
        elif argument in {"--nixpkgs", "--source"}:
            redacted.append(argument)
            hide_next = True
        elif argument.startswith("--nixpkgs=") or argument.startswith("--source="):
            redacted.append(argument.split("=", 1)[0] + "=<local-nixpkgs-checkout>")
        else:
            redacted.append(argument)
    return redacted


def build_casita(args: argparse.Namespace) -> None:
    if "casita" not in args.implementations or args.no_build:
        return
    profile = "debug" if args.casita_bin.parts[-3:-1] == ("target", "debug") else "release"
    command = ["cargo", "build", "--release", "--features", "cli", "--bin", "casita"]
    if profile == "release":
        command.insert(2, "--release")
    print(f"building Casita: {shlex.join(command)}", flush=True)
    run_checked(command)


def balanced_jobs(
    corpora: Sequence[str], operations: Sequence[str], policies: Sequence[str], adapters: dict[str, Adapter], repetitions: int, seed: int
) -> list[tuple[str, str, str, str, int]]:
    rng = random.Random(seed)
    jobs: list[tuple[str, str, str, str, int]] = []
    for corpus in corpora:
        for policy in policies:
            for operation in operations:
                names = [name for name, adapter in adapters.items() if operation in adapter.supported_operations]
                rng.shuffle(names)
                for repetition in range(repetitions):
                    rotated = names[repetition % len(names) :] + names[: repetition % len(names)] if names else []
                    jobs.extend((corpus, policy, operation, name, repetition) for name in rotated)
    return jobs


def write_atomic(path: pathlib.Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(content)
    temporary.replace(path)


def captured_output(path: pathlib.Path, limit: int = 64 * 1024) -> dict[str, object]:
    data = path.read_bytes()
    truncated = len(data) > limit
    if truncated:
        data = data[-limit:]
    return {"text": data.decode(errors="replace"), "truncated": truncated}


def remove_workspace(path: pathlib.Path) -> None:
    last_error: OSError | None = None
    for attempt in range(8):
        try:
            shutil.rmtree(path)
            return
        except FileNotFoundError:
            return
        except OSError as error:
            last_error = error
            time.sleep(0.05 * (attempt + 1))
    if last_error is not None:
        raise last_error


def validate_result(result: dict[str, object]) -> None:
    required = {"schema_version", "environment", "configuration", "tools", "corpora", "samples", "aggregates"}
    missing = required - set(result)
    if missing:
        raise BenchmarkError(f"result is missing required fields: {sorted(missing)}")
    if result["schema_version"] != SCHEMA_VERSION:
        raise BenchmarkError(f"unsupported result schema version: {result['schema_version']}")
    if not isinstance(result["samples"], list) or not isinstance(result["aggregates"], list):
        raise BenchmarkError("result samples and aggregates must be arrays")
    sample_fields = {
        "execution_index",
        "corpus",
        "cache_policy",
        "operation",
        "implementation",
        "repetition",
        "source_bytes",
        "status",
    }
    for index, sample in enumerate(result["samples"]):
        if not isinstance(sample, dict):
            raise BenchmarkError(f"sample {index} is not an object")
        sample_missing = sample_fields - set(sample)
        if sample_missing:
            raise BenchmarkError(f"sample {index} is missing fields: {sorted(sample_missing)}")
        if sample["status"] not in ("ok", "failed"):
            raise BenchmarkError(f"sample {index} has invalid status: {sample['status']}")
        if sample["status"] == "ok":
            timed_fields = {
                "command",
                "wall_seconds",
                "user_seconds",
                "system_seconds",
                "max_rss_bytes",
                "repository_usage",
                "throughput_bytes_per_second",
                "stdout",
                "stderr",
                "operation_metrics",
            }
            timed_missing = timed_fields - set(sample)
            if timed_missing:
                raise BenchmarkError(f"successful sample {index} is missing fields: {sorted(timed_missing)}")
    # Serialization is part of the contract: reject accidental non-JSON values
    # before either output file is replaced.
    json.dumps(result, allow_nan=False)


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.render_existing is not None:
            result = json.loads(args.render_existing.read_text())
            if not isinstance(result, dict):
                raise BenchmarkError("raw benchmark result must be a JSON object")
            validate_result(result)
            report = args.report or args.render_existing.with_suffix(".md")
            write_atomic(report, render_report(result))
            print(f"report: {report}")
            if args.html is not None:
                write_atomic(args.html, render_html(result))
                print(f"html: {args.html}")
            return 0
        normalize_args(args)
        temporary: tempfile.TemporaryDirectory[str] | None = None
        if args.keep_work:
            work_root = args.keep_work.resolve()
            work_root.mkdir(parents=True, exist_ok=True)
        else:
            temporary = tempfile.TemporaryDirectory(prefix="casita-benchmark-")
            work_root = pathlib.Path(temporary.name)
        metadata = environment_metadata(work_root)
        if args.require_clean and metadata["casita_worktree_dirty"]:
            raise BenchmarkError("the Casita worktree is dirty; commit or stash changes, or omit --require-clean")
        if args.casita_revision:
            metadata["harness_revision"] = metadata["casita_revision"]
            metadata["harness_worktree_dirty"] = metadata["casita_worktree_dirty"]
            metadata["casita_revision"] = args.casita_revision
            metadata["casita_worktree_dirty"] = False
        build_casita(args)
        corpus_root = work_root / "corpora"
        generated = {
            name: generate_corpus(corpus_root, name, SCALES[args.profile][name])
            for name in args.corpora
            if name in CORPORA
        }
        corpus_metadata: dict[str, dict[str, object]] = {}
        if "nixpkgs" in args.corpora:
            nixpkgs, source_metadata = materialize_nixpkgs_corpus(
                args.nixpkgs,
                args.nixpkgs_revision,
                corpus_root,
            )
            generated["nixpkgs"] = nixpkgs
            corpus_metadata["nixpkgs"] = source_metadata
        adapters, skipped = make_adapters(args)
        jobs = balanced_jobs(args.corpora, args.operations, args.cache_policies, adapters, args.repetitions, args.seed)
        samples: list[dict[str, object]] = []
        cache_details: dict[str, str] = {}
        total = len(jobs)
        for index, (corpus_name, policy, operation, implementation, repetition) in enumerate(jobs, start=1):
            print(f"[{index}/{total}] {corpus_name} {policy} {operation} {implementation} repetition={repetition + 1}", flush=True)
            workspace = work_root / "work" / f"{index:05d}-{corpus_name}-{policy}-{operation}-{implementation}-r{repetition + 1}"
            workspace.mkdir(parents=True)
            corpus = generated[corpus_name]
            adapter = adapters[implementation]
            sample: dict[str, object] = {
                "execution_index": index,
                "corpus": corpus_name,
                "cache_policy": policy,
                "operation": operation,
                "implementation": implementation,
                "repetition": repetition + 1,
                "source_bytes": corpus.edited_bytes if operation in ("edited-import", "edited-import-in-place", "sync-warm", "collect") else corpus.base_bytes,
            }
            try:
                prepared = prepare_sample(adapter, operation, corpus, workspace)
                sample["command"] = prepared.command.display()
                cache_details[policy] = apply_cache_policy(policy, prepared.cache_paths)
                stdout_path = workspace / "timed.stdout"
                stderr_path = workspace / "timed.stderr"
                timing = measured_command(prepared.command, stdout_path, stderr_path)
                sample.update(timing)
                sample["stdout"] = captured_output(stdout_path)
                sample["stderr"] = captured_output(stderr_path)
                stdout = sample["stdout"]
                assert isinstance(stdout, dict)
                sample["operation_metrics"] = adapter.operation_metrics(operation, str(stdout["text"]))
                sample["repository_usage"] = filesystem_usage(prepared.repository_paths)
                sample["storage_metrics"] = adapter.storage_metrics(prepared.repository_paths)
                sample["throughput_bytes_per_second"] = float(sample["source_bytes"]) / float(sample["wall_seconds"])
                adapter.validate(operation, corpus, workspace, prepared)
                sample["status"] = "ok"
            except (BenchmarkError, OSError, StopIteration) as error:
                sample["status"] = "failed"
                sample["error"] = str(error)
                print(f"  FAILED: {error}", file=sys.stderr, flush=True)
            samples.append(sample)
            if args.keep_work is None:
                remove_workspace(workspace)

        result: dict[str, object] = {
            "result_schema": "casita.repository-e2e.v1",
            "suite_id": "repository-e2e",
            "schema_version": SCHEMA_VERSION,
            "environment": metadata,
            "configuration": {
                "profile": args.profile,
                "corpora": args.corpora,
                "operations": args.operations,
                "implementations": args.implementations,
                "cache_policies": args.cache_policies,
                "cache_control": cache_details,
                "repetitions": args.repetitions,
                "interleaving_seed": args.seed,
                "source_date_epoch": SOURCE_DATE_EPOCH,
                "casita_pack_target_bytes": args.casita_pack_target_bytes,
                "casita_fsck_mode": args.casita_fsck_mode,
                "casita_incremental_sync": args.casita_incremental_sync,
                "skip_post_fsck": args.skip_post_fsck,
                "git_pack_comparator": args.git_pack_comparator,
                "argv": redacted_argv(
                    list(sys.argv if argv is None else [sys.argv[0], *argv])
                ),
            },
            "tools": tool_versions(adapters, skipped),
            "corpora": {
                name: {
                    "base_bytes": corpus.base_bytes,
                    "edited_bytes": corpus.edited_bytes,
                    "base_manifest_sha256": manifest_identity(corpus.base_manifest),
                    "edited_manifest_sha256": manifest_identity(corpus.edited_manifest),
                    **corpus_metadata.get(name, {}),
                }
                for name, corpus in generated.items()
            },
            "samples": samples,
        }
        result["aggregates"] = aggregates(samples)
        validate_result(result)
        write_atomic(args.output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        write_atomic(args.report, render_report(result))
        if args.html is not None:
            write_atomic(args.html, render_html(result))
        print(f"raw results: {args.output}")
        print(f"report: {args.report}")
        if args.html is not None:
            print(f"html: {args.html}")
        failures = sum(sample["status"] != "ok" for sample in samples)
        if temporary is not None:
            temporary.cleanup()
        return 1 if failures else 0
    except (BenchmarkError, argparse.ArgumentTypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
