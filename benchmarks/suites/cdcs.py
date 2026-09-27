#!/usr/bin/env python3
"""Content-defined chunk slicing on real rebuilt trees.

Two directory trees with the same file layout, typically two rebuilds of one
Nix store path that differ only in the embedded store hashes of their
dependencies, are encoded by the permanent `cdcs` Criterion target under
every exact-chunk and slicing strategy. The target reconstructs every blob
from its compressed representation and checks the BLAKE3 identity before it
emits a sample, so a row without `correctness: passed` never reaches a result.

Pairs are either given explicitly or discovered in a local Nix store: among
all paths named `<hash>-NAME`, the first two with identical file layouts but
different contents form the corpus. Identical copies and different layouts are
skipped so the measurement never reports a zero-change or a version upgrade as
a rebuild.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
from collections.abc import Sequence
from typing import Any

from benchmarks.lib.redact import local_paths
from benchmarks.suites import repository as common


SCHEMA = "casita.cdcs.v1"
RESULT_SCHEMA = "casita.cdcs-corpus.v1"
STRATEGIES = (
    "exact/256KiB",
    "exact/64KiB",
    "exact/8KiB",
    "exact/1KiB",
    "slices/8KiB/16",
    "slices/2KiB/16",
    "slices/1KiB/16",
    "slices/1KiB/4",
    "slices/1KiB/1",
)
DEFAULT_STORE = pathlib.Path("/nix/store")
STORE_HASH_LENGTH = 32


class CdcsBenchmarkError(RuntimeError):
    pass


def parse_benchmark_binary(output: str) -> pathlib.Path:
    executables = []
    for line in output.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = message.get("target", {})
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and target.get("name") == "cdcs"
            and "bench" in target.get("kind", [])
            and executable
        ):
            executables.append(pathlib.Path(executable))
    if len(executables) != 1:
        raise CdcsBenchmarkError(
            f"cargo emitted {len(executables)} cdcs benchmark executables; expected one"
        )
    return executables[0]


def build_benchmark_binary() -> pathlib.Path:
    completed = subprocess.run(
        [
            "cargo",
            "bench",
            "--features",
            "experimental",
            "--bench",
            "cdcs",
            "--no-run",
            "--message-format=json",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise CdcsBenchmarkError(completed.stderr or completed.stdout)
    binary = parse_benchmark_binary(completed.stdout)
    if not binary.is_file():
        raise CdcsBenchmarkError(f"benchmark executable does not exist: {binary}")
    return binary.resolve()


def tree_layout(root: pathlib.Path) -> list[tuple[str, str, object]]:
    """Every entry below `root` with its kind and size or link target.

    Two trees with equal layouts have the same relative paths, kinds, regular
    file sizes, and symlink targets, which is what a hash-only rebuild keeps.
    """
    layout: list[tuple[str, str, object]] = []
    for directory, names, files in os.walk(root):
        names.sort()
        for name in sorted([*names, *files]):
            path = pathlib.Path(directory) / name
            relative = str(path.relative_to(root))
            if path.is_symlink():
                layout.append((relative, "symlink", os.readlink(path)))
            elif path.is_dir():
                layout.append((relative, "dir", None))
            elif path.is_file():
                layout.append((relative, "file", path.stat().st_size))
            else:
                layout.append((relative, "other", None))
    return layout


def layout_identity(layout: Sequence[tuple[str, str, object]]) -> str:
    return hashlib.sha256(json.dumps(layout, sort_keys=True).encode()).hexdigest()


def content_identity(root: pathlib.Path, layout: Sequence[tuple[str, str, object]]) -> str:
    digest = hashlib.sha256()
    for relative, kind, _ in layout:
        if kind != "file":
            continue
        digest.update(relative.encode())
        digest.update(b"\0")
        with open(root / relative, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        digest.update(b"\0")
    return digest.hexdigest()


def store_candidates(store: pathlib.Path, name: str) -> list[pathlib.Path]:
    candidates = []
    for path in store.iterdir():
        if (
            len(path.name) > STORE_HASH_LENGTH + 1
            and path.name[STORE_HASH_LENGTH] == "-"
            and path.name[STORE_HASH_LENGTH + 1 :] == name
            and path.is_dir()
            and not path.is_symlink()
        ):
            candidates.append(path)
    return sorted(candidates)


def discover_pair(store: pathlib.Path, name: str) -> dict[str, Any]:
    """The first pair of same-layout, different-content store paths for `name`."""
    candidates = store_candidates(store, name)
    if len(candidates) < 2:
        raise CdcsBenchmarkError(
            f"{store} holds {len(candidates)} paths named {name}; a pair is required"
        )
    layouts: dict[str, list[pathlib.Path]] = {}
    layout_by_path: dict[pathlib.Path, list[tuple[str, str, object]]] = {}
    for candidate in candidates:
        layout = tree_layout(candidate)
        layout_by_path[candidate] = layout
        layouts.setdefault(layout_identity(layout), []).append(candidate)
    identical_copies = 0
    for group in layouts.values():
        if len(group) < 2:
            continue
        contents: dict[pathlib.Path, str] = {}
        for index, base in enumerate(group):
            contents.setdefault(base, content_identity(base, layout_by_path[base]))
            for rebuilt in group[index + 1 :]:
                contents.setdefault(rebuilt, content_identity(rebuilt, layout_by_path[rebuilt]))
                if contents[base] == contents[rebuilt]:
                    identical_copies += 1
                    continue
                return {
                    "name": name,
                    "base": str(base),
                    "rebuilt": str(rebuilt),
                    "candidates": len(candidates),
                    "layouts": len(layouts),
                    "identical_pairs_skipped": identical_copies,
                    "files": sum(1 for _, kind, _ in layout_by_path[base] if kind == "file"),
                    "bytes": sum(
                        int(size) for _, kind, size in layout_by_path[base] if kind == "file"
                    ),
                }
    raise CdcsBenchmarkError(
        f"no two paths named {name} share a layout with different contents "
        f"({len(candidates)} candidates, {len(layouts)} layouts)"
    )


def explicit_pair(base: pathlib.Path, rebuilt: pathlib.Path) -> dict[str, Any]:
    base_layout = tree_layout(base)
    if layout_identity(base_layout) != layout_identity(tree_layout(rebuilt)):
        raise CdcsBenchmarkError(
            f"{base} and {rebuilt} have different layouts; slicing measures rebuilds of one tree"
        )
    return {
        "name": base.name,
        "base": str(base),
        "rebuilt": str(rebuilt),
        "candidates": 2,
        "layouts": 1,
        "identical_pairs_skipped": 0,
        "files": sum(1 for _, kind, _ in base_layout if kind == "file"),
        "bytes": sum(int(size) for _, kind, size in base_layout if kind == "file"),
    }


def store_name(path: pathlib.Path) -> str:
    return path.name[STORE_HASH_LENGTH + 1 :]


def package_key(name: str) -> str:
    """The name with its first version segment removed, so `firefox-155.0.1`
    and `firefox-155.0.0` share a key while `gcc-15.3.0-lib` stays distinct
    from `gcc-15.3.0`."""
    return re.sub(r"-[0-9][^-]*", "", name, count=1)


def closure_requisites(root: pathlib.Path) -> list[pathlib.Path]:
    completed = subprocess.run(
        ["nix-store", "--query", "--requisites", str(root)],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise CdcsBenchmarkError(completed.stderr or completed.stdout)
    return sorted(pathlib.Path(line) for line in completed.stdout.split() if line)


def tree_summary(root: pathlib.Path) -> tuple[int, int, list[tuple[str, str, object]]]:
    if root.is_file() and not root.is_symlink():
        size = root.stat().st_size
        return 1, size, [(root.name, "file", size)]
    layout = tree_layout(root)
    files = sum(1 for _, kind, _ in layout if kind == "file")
    size = sum(int(size) for _, kind, size in layout if kind == "file")
    return files, size, layout


def closure_pairs(
    base_root: pathlib.Path,
    rebuilt_root: pathlib.Path,
    requisites=closure_requisites,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Pair every store path that is new in the rebuilt closure with its old
    counterpart: the same name (a rebuild) or, failing that, the unique old
    path with the same package key (an upgrade). Paths shared by both closures
    are already stored and cost nothing under any strategy; derivations and
    paths without a counterpart are counted but not measured."""
    base_paths = requisites(base_root)
    rebuilt_paths = requisites(rebuilt_root)
    shared = set(base_paths) & set(rebuilt_paths)
    gone = [path for path in base_paths if path not in shared and not path.name.endswith(".drv")]
    delta = [path for path in rebuilt_paths if path not in shared and not path.name.endswith(".drv")]
    by_name: dict[str, list[pathlib.Path]] = {}
    by_key: dict[str, list[pathlib.Path]] = {}
    for path in gone:
        by_name.setdefault(store_name(path), []).append(path)
        by_key.setdefault(package_key(store_name(path)), []).append(path)
    pairs: list[dict[str, Any]] = []
    unpaired: list[dict[str, Any]] = []
    empty: list[str] = []
    for path in delta:
        name = store_name(path)
        if name in by_name:
            counterpart, category = by_name[name][0], "rebuild"
        elif len(by_key.get(package_key(name), [])) == 1:
            counterpart, category = by_key[package_key(name)][0], "upgrade"
        else:
            files, size, _ = tree_summary(path)
            unpaired.append({"path": str(path), "files": files, "bytes": size})
            continue
        files, size, layout = tree_summary(path)
        if files == 0:
            # Symlink-only trees hold no blobs; the encoder has nothing to measure.
            empty.append(str(path))
            continue
        base_files, base_size, base_layout = tree_summary(counterpart)
        pairs.append(
            {
                "name": name,
                "category": category,
                "base": str(counterpart),
                "rebuilt": str(path),
                "files": files,
                "bytes": size,
                "base_files": base_files,
                "base_bytes": base_size,
                "layout_equal": layout_identity(layout) == layout_identity(base_layout),
            }
        )
    summary = {
        "base_root": str(base_root),
        "rebuilt_root": str(rebuilt_root),
        "base_paths": len(base_paths),
        "rebuilt_paths": len(rebuilt_paths),
        "shared_paths": len(shared),
        "measured_pairs": len(pairs),
        "measured_bytes": sum(pair["bytes"] for pair in pairs),
        "rebuild_pairs": sum(1 for pair in pairs if pair["category"] == "rebuild"),
        "upgrade_pairs": sum(1 for pair in pairs if pair["category"] == "upgrade"),
        "unpaired": unpaired,
        "unpaired_bytes": sum(entry["bytes"] for entry in unpaired),
        "empty": empty,
    }
    return pairs, summary


WIRE_SCHEMA = "casita.sliced-wire.v1"
CLOSURE_WIRE_SCHEMA = "casita.sliced-closure.v1"
# A rebuild that has the old generation to slice against must cost a fraction
# of a cold sync. Measured at an eighth; the gate leaves room for corpus drift
# and still catches a payload that stops being sliced, which cost a third.
CLOSURE_WIRE_MAX_REBUILD_SHARE = 0.25
# A sync costs a few requests whatever its size: the handshake, the discovery
# answer, and the offer of bases. Above that, a closure that needs one request
# per root has stopped travelling in batches. 349 paths cost 134 requests.
CLOSURE_WIRE_FIXED_REQUESTS = 8
DEFAULT_LINKS = "0:0,12800:5,2560:50"


def parse_links(spec: str) -> list[tuple[int, int]]:
    """`KIB:MS` pairs: bandwidth in KiB/s (0 unpaced) and round trip in ms."""
    links = []
    for entry in spec.split(","):
        entry = entry.strip()
        if not entry:
            continue
        bandwidth, _, rtt = entry.partition(":")
        try:
            links.append((int(bandwidth), int(rtt or 0)))
        except ValueError as error:
            raise CdcsBenchmarkError(f"invalid link {entry!r}: {error}") from None
    if not links:
        raise CdcsBenchmarkError("at least one link is required")
    return links


def build_example(name: str) -> pathlib.Path:
    completed = subprocess.run(
        [
            "cargo",
            "build",
            "--profile",
            "bench",
            "--features",
            "ssh,experimental",
            "--example",
            name,
            "--message-format=json",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise CdcsBenchmarkError(completed.stderr or completed.stdout)
    executables = []
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = message.get("target", {})
        if (
            message.get("reason") == "compiler-artifact"
            and target.get("name") == name
            and "example" in target.get("kind", [])
            and message.get("executable")
        ):
            executables.append(pathlib.Path(message["executable"]))
    if len(executables) != 1:
        raise CdcsBenchmarkError(
            f"cargo emitted {len(executables)} {name} executables; expected one"
        )
    return executables[0].resolve()


def parse_wire_rows(output: str) -> list[dict[str, Any]]:
    rows = []
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        row = json.loads(line)
        if row.get("schema") != WIRE_SCHEMA:
            continue
        if row.get("correctness") != "passed":
            raise CdcsBenchmarkError(f"wire phase {row.get('phase')} did not pass its gate")
        rows.append(row)
    if [row["phase"] for row in rows] != ["cold", "rebuild"]:
        raise CdcsBenchmarkError("the wire example must report a cold and a rebuild phase")
    return rows


def run_wire(
    binary: pathlib.Path,
    pair: dict[str, Any],
    links: Sequence[tuple[int, int]],
    workspace: pathlib.Path,
    stem: str,
) -> list[dict[str, Any]]:
    rows = []
    for bandwidth, rtt in links:
        stdout = workspace / f"{stem}-{bandwidth}-{rtt}.stdout"
        stderr = workspace / f"{stem}-{bandwidth}-{rtt}.stderr"
        command = common.CommandSpec(
            [[
                str(binary),
                "--base",
                pair["base"],
                "--rebuilt",
                pair["rebuilt"],
                "--bandwidth-kib",
                str(bandwidth),
                "--rtt-ms",
                str(rtt),
            ]],
            workspace,
            dict(os.environ),
        )
        common.measured_command(command, stdout, stderr)
        for row in parse_wire_rows(stdout.read_text(errors="replace")):
            row["pair"] = pair["name"]
            row["pair_path"] = pair["rebuilt"]
            rows.append(row)
    return rows


def render_wire_table(rows: Sequence[dict[str, Any]]) -> list[str]:
    if not rows:
        return []
    lines = [
        "## Over the wire",
        "",
        "Each pair is imported into a source and a destination repository and the",
        "rebuilt root is synced through the stdio transfer protocol behind a relay",
        "that delays each direction by half the round trip and paces it at the",
        "bandwidth: once into an empty destination and once into the destination",
        "that holds the base. Down is what the server sent; every destination",
        "closure verified complete.",
        "",
        "| Pair | Link | Phase | Down | Up | Requests | Copied | Literal | Wall |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        link = "unpaced" if not row["bandwidth_kib"] else f"{row['bandwidth_kib']} KiB/s, {row['rtt_ms']:.0f} ms"
        lines.append(
            f"| {row['pair']} | {link} | {row['phase']} | "
            f"{common.human_bytes(row['server_to_client_bytes'])} | "
            f"{common.human_bytes(row['client_to_server_bytes'])} | {row['transport_requests']} | "
            f"{common.human_bytes(row['slice_copy_bytes'])} | {common.human_bytes(row['slice_literal_bytes'])} | "
            f"{row['wall_seconds'] * 1000:.0f} ms |"
        )
    lines.append("")
    return lines


def write_pairs_file(pairs, path: pathlib.Path) -> pathlib.Path:
    """The closure example reads `name base rebuilt` a line, as pairing emits."""
    path.write_text(
        "".join(f"{pair['name']} {pair['base']} {pair['rebuilt']}\n" for pair in pairs)
    )
    return path


def parse_closure_wire_rows(output: str, pair_count: int) -> list[dict[str, Any]]:
    rows = []
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        row = json.loads(line)
        if row.get("schema") != CLOSURE_WIRE_SCHEMA:
            continue
        if row.get("correctness") != "passed":
            raise CdcsBenchmarkError(
                f"closure wire phase {row.get('phase')} did not verify its destination"
            )
        rows.append(row)
    phases = [row["phase"] for row in rows]
    if phases != ["cold", "rebuild"]:
        raise CdcsBenchmarkError(
            f"the closure example must report a cold and a rebuild phase, got {phases}"
        )
    cold, rebuild = rows
    share = rebuild["server_to_client_bytes"] / max(cold["server_to_client_bytes"], 1)
    if share > CLOSURE_WIRE_MAX_REBUILD_SHARE:
        raise CdcsBenchmarkError(
            f"the rebuild sent {share:.0%} of the cold sync's bytes; slicing should "
            f"keep it under {CLOSURE_WIRE_MAX_REBUILD_SHARE:.0%}"
        )
    # One request per rebuilt path would mean the closure stopped travelling in
    # batches. It costs about one request for every three paths.
    budget = pair_count + CLOSURE_WIRE_FIXED_REQUESTS
    if rebuild["transport_requests"] > budget:
        raise CdcsBenchmarkError(
            f"the rebuild made {rebuild['transport_requests']} requests for {pair_count} "
            f"paths; a closure travelling in batches costs at most {budget}"
        )
    return rows


def run_closure_wire(
    binary: pathlib.Path,
    pairs: Sequence[dict[str, Any]],
    links: Sequence[tuple[int, int]],
    workspace: pathlib.Path,
    max_bytes: int,
) -> list[dict[str, Any]]:
    """Move every rebuilt root of a closure as one transfer, over each link.

    The work directory is shared across links and reused when it already holds
    an import, because importing a closure costs far more than syncing it and
    neither the source nor the base generation is mutated by a sync.
    """
    pairs_file = write_pairs_file(pairs, workspace / "closure-pairs.txt")
    work = workspace / "closure-work"
    work.mkdir(parents=True, exist_ok=True)
    rows = []
    for bandwidth, rtt in links:
        stdout = workspace / f"closure-wire-{bandwidth}-{rtt}.stdout"
        stderr = workspace / f"closure-wire-{bandwidth}-{rtt}.stderr"
        command = common.CommandSpec(
            [[
                str(binary),
                "--pairs",
                str(pairs_file),
                "--work",
                str(work),
                "--max-bytes",
                str(max_bytes),
                "--bandwidth-kib",
                str(bandwidth),
                "--rtt-ms",
                str(rtt),
                "--reuse",
                "true",
            ]],
            workspace,
            dict(os.environ),
        )
        common.measured_command(command, stdout, stderr)
        rows.extend(parse_closure_wire_rows(stdout.read_text(errors="replace"), len(pairs)))
    return rows


def render_closure_wire_table(rows: Sequence[dict[str, Any]]) -> list[str]:
    if not rows:
        return []
    lines = [
        "## A whole closure over the wire",
        "",
        "Every rebuilt root of the closure moves as one transfer request, behind a",
        "relay that delays each direction by half the round trip and paces it at the",
        "bandwidth. Cold syncs into an empty destination, rebuild into one holding",
        "the old generation. Down is what the server sent; every destination closure",
        "verified complete.",
        "",
        "| Link | Phase | Paths | Down | Requests | Copied | Literal | Wall |",
        "|---|---|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        link = (
            "unpaced"
            if not row["bandwidth_kib"]
            else f"{row['bandwidth_kib']} KiB/s, {row['rtt_ms']:.0f} ms"
        )
        lines.append(
            f"| {link} | {row['phase']} | {row['pairs']} | "
            f"{common.human_bytes(row['server_to_client_bytes'])} | "
            f"{row['transport_requests']} | "
            f"{common.human_bytes(row['slice_copy_bytes'])} | "
            f"{common.human_bytes(row['slice_literal_bytes'])} | "
            f"{row['wall_seconds']:.1f} s |"
        )
    lines.append("")
    return lines


def parse_samples(output: str) -> list[dict[str, Any]]:
    samples = []
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        row = json.loads(line)
        if row.get("schema") != SCHEMA:
            continue
        if row.get("correctness") != "passed":
            raise CdcsBenchmarkError(f"strategy {row.get('strategy')} did not pass its gate")
        samples.append(row)
    strategies = [row["strategy"] for row in samples]
    if strategies != list(STRATEGIES):
        raise CdcsBenchmarkError(
            f"expected strategies {list(STRATEGIES)}, benchmark reported {strategies}"
        )
    return samples


def run_pair(
    binary: pathlib.Path,
    pair: dict[str, Any],
    repetition: int,
    workspace: pathlib.Path,
    *,
    stem: str | None = None,
    compact: bool = False,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    environment = {
        **os.environ,
        "CASITA_CDCS_BASE": pair["base"],
        "CASITA_CDCS_REBUILT": pair["rebuilt"],
    }
    environment.pop("CASITA_CDCS_REPORT", None)
    stem = f"{stem or pair['name']}-{repetition:02d}"
    stdout = workspace / f"{stem}.stdout"
    stderr = workspace / f"{stem}.stderr"
    command = common.CommandSpec([[str(binary), "--test"]], workspace, environment)
    process = common.measured_command(command, stdout, stderr)
    samples = parse_samples(stderr.read_text(errors="replace"))
    for sample in samples:
        sample["pair"] = pair["name"]
        sample["pair_path"] = pair["rebuilt"]
        sample["repetition"] = repetition
        sample["process_max_rss_bytes"] = process["max_rss_bytes"]
        if compact:
            # A closure run has thousands of rows; keep the outcome only.
            sample.pop("corpus_description", None)
    record = {**process, "pair": pair["name"], "repetition": repetition}
    if compact:
        record["pair_index"] = stem
    else:
        record.update(
            command=command.display(),
            stdout=common.captured_output(stdout),
            stderr=common.captured_output(stderr),
        )
    if compact:
        stdout.unlink(missing_ok=True)
        stderr.unlink(missing_ok=True)
    return samples, record


def strategy_totals(result: dict[str, Any]) -> list[dict[str, Any]]:
    totals: dict[str, dict[str, Any]] = {}
    for sample in result["samples"]:
        outcome = sample["outcome"]
        total = totals.setdefault(
            sample["strategy"],
            {
                "strategy": sample["strategy"],
                "logical_bytes": 0,
                "physical_bytes": 0,
                "index_entries": 0,
                "encode_seconds": 0.0,
                "max_depth": 0,
                "max_sources_per_group": 0,
            },
        )
        total["logical_bytes"] += outcome["logical_bytes"]
        total["physical_bytes"] += outcome["physical_bytes"]
        total["index_entries"] += outcome["index_entries"]
        total["encode_seconds"] += outcome["encode_seconds"]
        total["max_depth"] = max(total["max_depth"], outcome["max_depth"])
        total["max_sources_per_group"] = max(
            total["max_sources_per_group"], outcome["max_sources_per_group"]
        )
    return [totals[strategy] for strategy in STRATEGIES if strategy in totals]


def render_closure_report(result: dict[str, Any]) -> str:
    closure = result["closure"]
    lines = [
        "# Content-defined chunk slicing on a rebuilt closure",
        "",
        f"Base closure `{closure['base_root']}` ({closure['base_paths']} paths), rebuilt closure "
        f"`{closure['rebuilt_root']}` ({closure['rebuilt_paths']} paths); {closure['shared_paths']} "
        f"paths are shared and cost nothing. {closure['measured_pairs']} rebuilt paths "
        f"({closure['rebuild_pairs']} same-name rebuilds, {closure['upgrade_pairs']} upgrades), "
        f"{common.human_bytes(closure['measured_bytes'])}, were encoded against their old "
        f"counterparts; {len(closure['unpaired'])} paths ({common.human_bytes(closure['unpaired_bytes'])}) "
        "have no counterpart and were not measured.",
        "",
        "Every row reconstructs each rebuilt file from its compressed encoding and",
        "checks the BLAKE3 identity before it is reported. Physical bytes are the",
        "new payload plus manifest or token metadata.",
        "",
        "| Strategy | Physical | Percent | Index entries | Max depth | Max sources/group | Encode seconds |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for total in strategy_totals(result):
        percent = total["physical_bytes"] * 100 / max(total["logical_bytes"], 1)
        lines.append(
            f"| {total['strategy']} | {common.human_bytes(total['physical_bytes'])} | {percent:.3f} | "
            f"{total['index_entries']:,} | {total['max_depth']} | {total['max_sources_per_group']} | "
            f"{total['encode_seconds']:.1f} |"
        )
    # Several store paths can share one name, so rows are keyed by path.
    by_pair: dict[str, dict[str, int]] = {}
    for sample in result["samples"]:
        by_pair.setdefault(sample["pair_path"], {})[sample["strategy"]] = sample["outcome"]["physical_bytes"]
    for category in ("rebuild", "upgrade"):
        pairs = [pair for pair in result["pairs"] if pair["category"] == category]
        if not pairs:
            continue
        logical = sum(pair["bytes"] for pair in pairs)
        lines.extend(["", f"## {category} ({len(pairs)} pairs, {common.human_bytes(logical)})", ""])
        lines.append("| Strategy | Physical | Percent |")
        lines.append("|---|---:|---:|")
        for strategy in STRATEGIES:
            physical = sum(by_pair.get(pair["rebuilt"], {}).get(strategy, 0) for pair in pairs)
            lines.append(
                f"| {strategy} | {common.human_bytes(physical)} | {physical * 100 / max(logical, 1):.3f} |"
            )
    shown = ("exact/256KiB", "exact/8KiB", "slices/1KiB/4", "slices/1KiB/1")
    lines.extend(["", "## Largest rebuilt paths", ""])
    lines.append("| Path | Category | Bytes | " + " | ".join(shown) + " |")
    lines.append("|---|---|---:|" + "---:|" * len(shown))
    for pair in sorted(result["pairs"], key=lambda pair: pair["bytes"], reverse=True)[:20]:
        cells = [
            f"{by_pair.get(pair['rebuilt'], {}).get(strategy, 0) * 100 / max(pair['bytes'], 1):.2f}%"
            for strategy in shown
        ]
        lines.append(
            f"| {pair['name']} | {pair['category']} | {common.human_bytes(pair['bytes'])} | "
            + " | ".join(cells)
            + " |"
        )
    lines.extend(
        [
            "",
            "Each rebuilt path is encoded against its own old counterpart only, not the",
            "whole old closure, so cross-package matches are not counted. Exact rows",
            "model the current chunked backend; slicing rows model an encoder that does",
            "not exist in Casita. Timings are single-process wall-clock measurements of",
            "an in-memory encoder and exclude storage.",
            "",
        ]
    )
    lines.extend(render_closure_wire_table(result.get("closure_wire", [])))
    return "\n".join(lines)


def render_report(result: dict[str, Any]) -> str:
    lines = [
        "# Content-defined chunk slicing on rebuilt trees",
        "",
        "Every row reconstructs each rebuilt file from its compressed encoding and",
        "checks the BLAKE3 identity before it is reported. Physical bytes are the",
        "new payload plus manifest or token metadata; the base tree is already",
        "stored. Sources per 16 KiB group is the number of distinct objects a",
        "verified range read of that group must touch.",
        "",
    ]
    for pair in result["pairs"]:
        lines.extend(
            [
                f"## {pair['name']}",
                "",
                f"Base `{pair['base']}`, rebuilt `{pair['rebuilt']}`: "
                f"{pair['files']} regular files, {common.human_bytes(pair['bytes'])}; "
                f"{pair['candidates']} candidates in {pair['layouts']} layouts, "
                f"{pair['identical_pairs_skipped']} identical pairs skipped.",
                "",
                "| Strategy | Repetition | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources/group (mean, max) | Encode seconds |",
                "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for sample in result["samples"]:
            if sample["pair"] != pair["name"]:
                continue
            outcome = sample["outcome"]
            lines.append(
                "| {strategy} | {repetition} | {physical} | {percent:.3f} | {literal} | {copies} | "
                "{literals} | {depth} | {index} | {mean:.2f}, {max} | {seconds:.3f} |".format(
                    strategy=sample["strategy"],
                    repetition=sample["repetition"],
                    physical=common.human_bytes(outcome["physical_bytes"]),
                    percent=outcome["physical_percent"],
                    literal=common.human_bytes(outcome["literal_bytes"]),
                    copies=outcome["copies"],
                    literals=outcome["literals"],
                    depth=outcome["max_depth"],
                    index=outcome["index_entries"],
                    mean=outcome["mean_sources_per_group"],
                    max=outcome["max_sources_per_group"],
                    seconds=outcome["encode_seconds"],
                )
            )
        lines.append("")
    lines.extend(render_wire_table(result.get("wire", [])))
    lines.extend(
        [
            "Exact rows model the current chunked backend; slicing rows model the",
            "encoder the SSH transfer uses on the wire. Timings are single-process",
            "wall-clock measurements of an in-memory encoder and exclude storage.",
            "",
        ]
    )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=pathlib.Path, help="explicit base tree")
    parser.add_argument("--rebuilt", type=pathlib.Path, help="explicit rebuilt tree")
    parser.add_argument(
        "--store-name",
        action="append",
        default=[],
        help="discover a rebuilt pair for this <hash>-NAME in the Nix store; repeatable",
    )
    parser.add_argument("--store", type=pathlib.Path, default=DEFAULT_STORE)
    parser.add_argument(
        "--closure-base",
        type=pathlib.Path,
        help="a store path or profile link; every path new in --closure-rebuilt is paired with its old counterpart",
    )
    parser.add_argument("--closure-rebuilt", type=pathlib.Path)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument(
        "--wire",
        action="store_true",
        help="also sync every pair through the stdio transfer over each --links entry",
    )
    parser.add_argument(
        "--links",
        default=DEFAULT_LINKS,
        help="comma-separated KIB:MS links for --wire; 0 KiB/s is unpaced",
    )
    parser.add_argument("--wire-bin", type=pathlib.Path, help="prebuilt sliced_wire example")
    parser.add_argument(
        "--closure-wire",
        action="store_true",
        help="also move the whole rebuilt closure as one transfer over each --links entry",
    )
    parser.add_argument(
        "--closure-wire-bytes",
        type=int,
        default=1_500_000_000,
        help="logical bytes of rebuilt paths to select for --closure-wire",
    )
    parser.add_argument(
        "--closure-wire-bin", type=pathlib.Path, help="prebuilt sliced_closure example"
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--benchmark-bin", type=pathlib.Path)
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    temporary: tempfile.TemporaryDirectory[str] | None = None
    try:
        if args.repetitions < 1:
            raise CdcsBenchmarkError("repetitions must be positive")
        if (args.base is None) != (args.rebuilt is None):
            raise CdcsBenchmarkError("--base and --rebuilt must be given together")
        if (args.closure_base is None) != (args.closure_rebuilt is None):
            raise CdcsBenchmarkError("--closure-base and --closure-rebuilt must be given together")
        if args.base is None and not args.store_name and args.closure_base is None:
            raise CdcsBenchmarkError(
                "give --base/--rebuilt, --closure-base/--closure-rebuilt, or at least one --store-name"
            )
        pairs = []
        closure = None
        if args.base is not None:
            pairs.append(explicit_pair(args.base.resolve(), args.rebuilt.resolve()))
        for name in args.store_name:
            pairs.append(discover_pair(args.store, name))
        if args.closure_base is not None:
            closure_pair_list, closure = closure_pairs(args.closure_base, args.closure_rebuilt)
            if not closure_pair_list:
                raise CdcsBenchmarkError("the rebuilt closure has no paths with an old counterpart")
            pairs.extend(closure_pair_list)
        if args.no_build:
            if args.benchmark_bin is None:
                raise CdcsBenchmarkError("--no-build requires --benchmark-bin")
            binary = args.benchmark_bin.resolve()
        else:
            print("building the cdcs benchmark", flush=True)
            binary = build_benchmark_binary()
        if not binary.is_file():
            raise CdcsBenchmarkError(f"benchmark executable does not exist: {binary}")

        timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        args.output = args.output or pathlib.Path("benchmarks/results") / f"cdcs-{timestamp}.json"
        args.report = args.report or args.output.with_suffix(".md")
        if args.keep_work:
            workspace = args.keep_work.resolve()
            workspace.mkdir(parents=True, exist_ok=True)
        else:
            temporary = tempfile.TemporaryDirectory(prefix="casita-cdcs-")
            workspace = pathlib.Path(temporary.name)

        samples: list[dict[str, Any]] = []
        processes: list[dict[str, Any]] = []
        compact = closure is not None
        for index, pair in enumerate(pairs, start=1):
            for repetition in range(1, args.repetitions + 1):
                print(
                    f"[{index}/{len(pairs)} {repetition}/{args.repetitions}] {pair['name']}: "
                    f"{pair['files']} files, {common.human_bytes(pair['bytes'])}",
                    flush=True,
                )
                measured, process = run_pair(
                    binary, pair, repetition, workspace, stem=f"{index:05d}-{pair['name']}", compact=compact
                )
                samples.extend(measured)
                processes.append(process)

        wire_rows: list[dict[str, Any]] = []
        wire_binary = None
        if args.wire:
            links = parse_links(args.links)
            if args.wire_bin is not None:
                wire_binary = args.wire_bin.resolve()
            elif args.no_build:
                raise CdcsBenchmarkError("--no-build requires --wire-bin with --wire")
            else:
                print("building the sliced_wire example", flush=True)
                wire_binary = build_example("sliced_wire")
            if not wire_binary.is_file():
                raise CdcsBenchmarkError(f"wire example does not exist: {wire_binary}")
            for index, pair in enumerate(pairs, start=1):
                print(f"[wire {index}/{len(pairs)}] {pair['name']}", flush=True)
                wire_rows.extend(
                    run_wire(wire_binary, pair, links, workspace, f"wire-{index:05d}-{pair['name']}")
                )

        closure_wire_rows: list[dict[str, Any]] = []
        closure_wire_binary = None
        if args.closure_wire:
            if closure is None:
                raise CdcsBenchmarkError(
                    "--closure-wire needs --closure-base and --closure-rebuilt"
                )
            links = parse_links(args.links)
            if args.closure_wire_bin is not None:
                closure_wire_binary = args.closure_wire_bin.resolve()
            elif args.no_build:
                raise CdcsBenchmarkError("--no-build requires --closure-wire-bin")
            else:
                print("building the sliced_closure example", flush=True)
                closure_wire_binary = build_example("sliced_closure")
            if not closure_wire_binary.is_file():
                raise CdcsBenchmarkError(
                    f"closure example does not exist: {closure_wire_binary}"
                )
            print(f"[closure wire] {len(pairs)} pairs over {len(links)} links", flush=True)
            closure_wire_rows = run_closure_wire(
                closure_wire_binary, pairs, links, workspace, args.closure_wire_bytes
            )

        result = {
            "result_schema": RESULT_SCHEMA,
            "suite_id": "core-primitives",
            "schema_version": 1,
            "environment": common.environment_metadata(workspace),
            "configuration": {
                "repetitions": args.repetitions,
                "store": str(args.store),
                "links": args.links if (args.wire or args.closure_wire) else None,
                "closure_wire_bytes": args.closure_wire_bytes if args.closure_wire else None,
                "argv": list(sys.argv if argv is None else [sys.argv[0], *argv]),
            },
            "tools": {
                "benchmark": str(binary),
                "wire": str(wire_binary) if wire_binary else None,
                "closure_wire": str(closure_wire_binary) if closure_wire_binary else None,
            },
            "pairs": pairs,
            "processes": processes,
            "samples": samples,
            "wire": wire_rows,
            "closure_wire": closure_wire_rows,
        }
        if closure is not None:
            result["closure"] = closure
            result["totals"] = strategy_totals(result)
        common.write_atomic(
            args.output, json.dumps(local_paths(result), indent=2, sort_keys=True) + "\n"
        )
        common.write_atomic(
            args.report,
            render_closure_report(result) if closure is not None else render_report(result),
        )
        print(f"raw results: {args.output}")
        print(f"report: {args.report}")
        return 0
    except (CdcsBenchmarkError, common.BenchmarkError, OSError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
