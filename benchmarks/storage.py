"""Inventory, preserve and manage benchmark artifacts without guessing what to delete."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import sys
import tarfile
import tempfile
import uuid

from benchmarks import cli


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def new_output():
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return cli.ROOT / "benchmarks/results" / f"{stamp}-{uuid.uuid4().hex[:8]}"


def retain_binary(source, destination):
    """Let Cargo validate build inputs, then deduplicate immutable output bytes."""
    sha = digest(source)
    cache = cli.ROOT / "benchmarks/artifacts" / sha
    cache.parent.mkdir(parents=True, exist_ok=True)
    if cache.exists():
        if digest(cache) != sha:
            raise RuntimeError(f"corrupt benchmark artifact: {cache}")
    else:
        with tempfile.NamedTemporaryFile(dir=cache.parent, delete=False) as stream:
            temporary = pathlib.Path(stream.name)
        try:
            shutil.copy2(source, temporary)
            if digest(temporary) != sha:
                raise RuntimeError(f"artifact changed while copying: {source}")
            temporary.chmod(0o555)
            os.replace(temporary, cache)
        finally:
            temporary.unlink(missing_ok=True)
    try:
        os.link(cache, destination)
    except OSError:
        # An explicitly chosen output may be on another filesystem.
        if destination.exists():
            raise
        shutil.copy2(cache, destination)
    return {"path": str(destination), "sha256": sha, "source": str(source), "shared_path": str(cache)}


def prepare_work(output):
    directory = cli.ROOT / "benchmarks/work" / uuid.uuid4().hex
    directory.mkdir(parents=True)
    (directory / ".benchmark-work.json").write_text(json.dumps({"output": str(output)}))
    return directory


def files(root):
    for directory, subdirs, names in os.walk(root, followlinks=False):
        for name in sorted(subdirs + names):
            path = pathlib.Path(directory) / name
            if path.is_symlink() or path.is_file():
                yield path


def inventory(root):
    groups = {}
    for path in files(root):
        relative = path.relative_to(root)
        name = relative.parts[0] if len(relative.parts) > 1 else "(files)"
        row = groups.setdefault(name, {"files": 0, "bytes": 0, "symlinks": 0, "kinds": {}, "categories": {}})
        row["files"] += 1
        if path.is_symlink():
            row["symlinks"] += 1
            continue
        size = path.stat().st_size
        row["bytes"] += size
        kind = path.suffix or "(no extension)"
        row["kinds"][kind] = row["kinds"].get(kind, 0) + size
        if path.suffix in {".json", ".jsonl", ".log", ".md", ".csv", ".tsv", ".svg", ".html"}:
            category = "measurements-logs-documents"
        elif ".git" in relative.parts:
            category = "git-data"
        else:
            with path.open("rb") as stream:
                magic = stream.read(4)
            if magic == b"\x7fELF" or magic[:2] == b"MZ" or magic in {
                b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe"}:
                category = "binaries"
            else:
                category = "fixtures-sources-other"
        row["categories"][category] = row["categories"].get(category, 0) + size
    # References are hints for review, never grounds for automatic deletion.
    evidence = []
    for folder in ("reports", "baselines"):
        for path in files(cli.ROOT / "benchmarks" / folder):
            if path.suffix in {".md", ".json", ".py", ".sh"} and not path.is_symlink():
                evidence.append((str(path.relative_to(cli.ROOT)), path.read_text(errors="replace")))
    for name, row in groups.items():
        relative = f"benchmarks/results/{name}"
        row["references"] = [path for path, body in evidence if relative in body or str(root / name) in body]
    return {"schema_version": 1, "root": str(root), "bytes": sum(row["bytes"] for row in groups.values()), "runs": groups}


def file_index(source):
    print(f"hashing source files: {source}", file=sys.stderr, flush=True)
    expected = {}
    for path in files(source):
        name = path.relative_to(source).as_posix()
        expected[name] = {"link": os.readlink(path)} if path.is_symlink() else {
            "sha256": digest(path), "bytes": path.stat().st_size}
    return expected


def archive(source, destination):
    """Keep every file and link; verify contents without extracting the archive."""
    source = source.resolve()
    destination = destination.absolute()
    if destination.resolve().is_relative_to(source):
        raise ValueError("archive destination must be outside the source directory")
    if not source.is_dir():
        raise ValueError("archive source must be a directory")
    if destination.exists() or destination.with_suffix(destination.suffix + ".json").exists():
        raise ValueError("archive and verification manifest must not already exist")
    expected = file_index(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    print(f"archiving {len(expected)} files and links: {destination}", file=sys.stderr, flush=True)
    with destination.open("xb") as stream:
        with tarfile.open(fileobj=stream, mode="w:gz", compresslevel=1, dereference=False) as bundle:
            bundle.add(source, arcname=".")
    return verify_archive(source, destination, expected)


def verify_archive(source, destination, expected=None):
    source, destination = source.resolve(), destination.absolute()
    if not source.is_dir():
        raise ValueError("verification source must be a directory")
    if expected is None:
        expected = file_index(source)
    actual = {}
    print(f"verifying archive: {destination}", file=sys.stderr, flush=True)
    with tarfile.open(destination, "r|gz") as bundle:
        for member in bundle:
            name = member.name.removeprefix("./")
            if member.issym():
                actual[name] = {"link": member.linkname}
            elif member.islnk():
                actual[name] = dict(actual[member.linkname.removeprefix("./")])
            elif member.isfile():
                with bundle.extractfile(member) as stream:
                    actual[name] = {"sha256": hashlib.file_digest(stream, "sha256").hexdigest(), "bytes": member.size}
            elif not member.isdir():
                raise ValueError(f"unsupported archive member: {name}")
    if actual != expected:
        raise ValueError("archive verification failed; source retained")
    verification = {"source": str(source), "archive": str(destination), "sha256": digest(destination),
                    "verified": True, "files": expected}
    destination.with_suffix(destination.suffix + ".json").write_text(json.dumps(verification, indent=2) + "\n")
    return {key: value for key, value in verification.items() if key != "files"}


def clean(apply=False):
    root = cli.ROOT / "benchmarks/work"
    candidates = []
    if root.is_symlink():
        raise ValueError("refusing a symlinked work directory")
    for directory in sorted(root.iterdir()) if root.exists() else []:
        if directory.is_symlink() or not directory.is_dir():
            continue
        marker = directory / ".benchmark-work.json"
        if marker.is_symlink() or not marker.is_file():
            continue
        try:
            output = pathlib.Path(json.loads(marker.read_text())["output"])
            ledger = json.loads((output / "execution.json").read_text())
            if not ledger.get("finished") or ledger.get("work_directory") != str(directory):
                continue
        except (OSError, ValueError, KeyError, TypeError):
            continue
        size = sum(p.stat().st_size for p in files(directory) if not p.is_symlink())
        candidates.append({"path": str(directory), "bytes": size})
        if apply:
            # Scratch can include diagnostic traces. Preserve it losslessly
            # before removal rather than guessing which extensions are evidence.
            destination = cli.ROOT / "benchmarks/archives" / f"work-{directory.name}-{uuid.uuid4().hex[:8]}.tar.gz"
            candidates[-1]["archive"] = archive(directory, destination)["archive"]
            shutil.rmtree(directory)
    return {"applied": apply, "bytes": sum(item["bytes"] for item in candidates), "directories": candidates}


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    scan = commands.add_parser("inventory", help="inventory local results and report references")
    scan.add_argument("--output", type=pathlib.Path, help="also save the inventory as JSON")
    pack = commands.add_parser("archive", help="create and verify a lossless archive; never delete the source")
    pack.add_argument("source", type=pathlib.Path)
    pack.add_argument("--output", type=pathlib.Path, required=True)
    cleanup = commands.add_parser("clean", help="preview marked scratch directories belonging to finished runs")
    cleanup.add_argument("--apply", action="store_true", help="archive and verify the listed scratch directories, then remove them")
    args = parser.parse_args(argv)
    try:
        if args.command == "inventory":
            result = inventory(cli.ROOT / "benchmarks/results")
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(result, indent=2) + "\n")
        elif args.command == "archive":
            result = archive(args.source, args.output)
        else:
            result = clean(args.apply)
        print(json.dumps(result, indent=2))
        return 0
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(2, f"error: {error}\n")
