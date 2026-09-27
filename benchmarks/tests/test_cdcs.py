import contextlib
import io
import json
import os
import pathlib
import stat
import tempfile
import unittest

from benchmarks.suites import cdcs as benchmark


def write_tree(root: pathlib.Path, files: dict[str, bytes], links: dict[str, str] | None = None) -> None:
    for relative, content in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
    for relative, target in (links or {}).items():
        (root / relative).parent.mkdir(parents=True, exist_ok=True)
        (root / relative).symlink_to(target)


def sample_row(strategy: str, physical: int = 10) -> dict[str, object]:
    return {
        "schema": benchmark.SCHEMA,
        "corpus": "directories",
        "strategy": strategy,
        "correctness": "passed",
        "outcome": {
            "physical_bytes": physical,
            "physical_percent": 1.5,
            "literal_bytes": 4,
            "copies": 2,
            "literals": 1,
            "max_depth": 1,
            "index_entries": 7,
            "mean_sources_per_group": 1.25,
            "max_sources_per_group": 2,
            "encode_seconds": 0.01,
        },
    }


class PairDiscoveryTests(unittest.TestCase):
    def test_first_same_layout_pair_with_different_contents_wins(self):
        with tempfile.TemporaryDirectory() as directory:
            store = pathlib.Path(directory)
            hashes = ["a" * 32, "b" * 32, "c" * 32, "d" * 32, "e" * 32]
            layout = {"bin/tool": b"/nix/store/" + b"x" * 32 + b"-dep", "share/doc": b"same"}
            # An unrelated layout (a version bump) is never paired.
            write_tree(store / f"{hashes[0]}-pkg-1.0", {**layout, "extra": b"new file"})
            # Two byte-identical copies must not count as a rebuild.
            write_tree(store / f"{hashes[1]}-pkg-1.0", layout, {"lib/link": "../bin/tool"})
            write_tree(store / f"{hashes[2]}-pkg-1.0", layout, {"lib/link": "../bin/tool"})
            rebuilt = {**layout, "bin/tool": b"/nix/store/" + b"y" * 32 + b"-dep"}
            write_tree(store / f"{hashes[3]}-pkg-1.0", rebuilt, {"lib/link": "../bin/tool"})
            # Same name prefix but a different package name is excluded.
            write_tree(store / f"{hashes[4]}-pkg-1.0-doc", layout)

            pair = benchmark.discover_pair(store, "pkg-1.0")

            self.assertEqual(pair["base"], str(store / f"{hashes[1]}-pkg-1.0"))
            self.assertEqual(pair["rebuilt"], str(store / f"{hashes[3]}-pkg-1.0"))
            self.assertEqual(pair["candidates"], 4)
            self.assertEqual(pair["layouts"], 2)
            self.assertEqual(pair["identical_pairs_skipped"], 1)
            self.assertEqual(pair["files"], 2)
            self.assertEqual(pair["bytes"], len(layout["bin/tool"]) + len(layout["share/doc"]))

    def test_discovery_rejects_identical_copies_and_layout_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            store = pathlib.Path(directory)
            write_tree(store / f"{'a' * 32}-pkg", {"file": b"one"})
            write_tree(store / f"{'b' * 32}-pkg", {"file": b"one"})
            write_tree(store / f"{'c' * 32}-pkg", {"file": b"one", "other": b"two"})
            with self.assertRaises(benchmark.CdcsBenchmarkError):
                benchmark.discover_pair(store, "pkg")
            with self.assertRaises(benchmark.CdcsBenchmarkError):
                benchmark.discover_pair(store, "missing")

    def test_explicit_pair_requires_equal_layouts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_tree(root / "base", {"a": b"1", "b": b"22"})
            write_tree(root / "rebuilt", {"a": b"9", "b": b"88"})
            write_tree(root / "grown", {"a": b"9", "b": b"888"})
            pair = benchmark.explicit_pair(root / "base", root / "rebuilt")
            self.assertEqual(pair["files"], 2)
            self.assertEqual(pair["bytes"], 3)
            with self.assertRaises(benchmark.CdcsBenchmarkError):
                benchmark.explicit_pair(root / "base", root / "grown")


class ClosurePairingTests(unittest.TestCase):
    def test_package_key_strips_the_first_version_segment(self):
        self.assertEqual(benchmark.package_key("firefox-155.0.1"), "firefox")
        self.assertEqual(benchmark.package_key("gcc-15.3.0-lib"), "gcc-lib")
        self.assertEqual(benchmark.package_key("50-coredump.conf"), "50-coredump.conf")
        self.assertNotEqual(benchmark.package_key("gcc-15.3.0"), benchmark.package_key("gcc-15.3.0-lib"))

    def test_closure_delta_pairs_rebuilds_and_upgrades_and_counts_the_rest(self):
        with tempfile.TemporaryDirectory() as directory:
            store = pathlib.Path(directory)
            old_hello = store / f"{'a' * 32}-hello-2.12"
            new_hello = store / f"{'b' * 32}-hello-2.12"
            old_fox = store / f"{'c' * 32}-firefox-155.0.0"
            new_fox = store / f"{'d' * 32}-firefox-155.0.1"
            shared = store / f"{'e' * 32}-glibc-2.42"
            brand_new = store / f"{'f' * 32}-newpkg-1.0"
            old_conf = store / f"{'g' * 32}-50-coredump.conf"
            new_conf = store / f"{'h' * 32}-50-coredump.conf"
            drv = store / f"{'i' * 32}-hello-2.12.drv"
            old_links = store / f"{'j' * 32}-links"
            new_links = store / f"{'k' * 32}-links"
            write_tree(old_links, {}, {"bin": "../" + old_hello.name + "/bin"})
            write_tree(new_links, {}, {"bin": "../" + new_hello.name + "/bin"})
            write_tree(old_hello, {"bin/hello": b"old"})
            write_tree(new_hello, {"bin/hello": b"new"})
            write_tree(old_fox, {"lib/x": b"1", "extra": b"2"})
            write_tree(new_fox, {"lib/x": b"3"})
            write_tree(shared, {"lib/libc": b"same"})
            write_tree(brand_new, {"bin/n": b"12345"})
            old_conf.write_bytes(b"conf")
            new_conf.write_bytes(b"conf2")
            drv.write_bytes(b"drv")
            closures = {
                "old": [old_hello, old_fox, shared, old_conf, old_links],
                "new": [new_hello, new_fox, shared, brand_new, new_conf, drv, new_links],
            }
            pairs, summary = benchmark.closure_pairs(
                pathlib.Path("old"), pathlib.Path("new"), requisites=lambda root: closures[root.name]
            )
            by_name = {pair["name"]: pair for pair in pairs}
            self.assertEqual(set(by_name), {"hello-2.12", "firefox-155.0.1", "50-coredump.conf"})
            self.assertEqual(by_name["hello-2.12"]["category"], "rebuild")
            self.assertEqual(by_name["hello-2.12"]["base"], str(old_hello))
            self.assertTrue(by_name["hello-2.12"]["layout_equal"])
            self.assertEqual(by_name["firefox-155.0.1"]["category"], "upgrade")
            self.assertFalse(by_name["firefox-155.0.1"]["layout_equal"])
            self.assertEqual(by_name["50-coredump.conf"]["files"], 1)
            self.assertEqual(by_name["50-coredump.conf"]["bytes"], 5)
            self.assertEqual(summary["shared_paths"], 1)
            self.assertEqual(summary["rebuild_pairs"], 2)
            self.assertEqual(summary["upgrade_pairs"], 1)
            self.assertEqual([entry["path"] for entry in summary["unpaired"]], [str(brand_new)])
            self.assertEqual(summary["unpaired_bytes"], 5)
            self.assertEqual(summary["empty"], [str(new_links)])
            self.assertEqual(summary["measured_bytes"], 3 + 1 + 5)

    def test_closure_report_totals_every_strategy(self):
        pairs = [
            {"name": "a", "category": "rebuild", "bytes": 100, "base": "/b", "rebuilt": "/a"},
            {"name": "b", "category": "upgrade", "bytes": 300, "base": "/c", "rebuilt": "/d"},
        ]
        samples = []
        for pair in pairs:
            for strategy in benchmark.STRATEGIES:
                row = sample_row(strategy, physical=pair["bytes"] // 10)
                row["outcome"]["logical_bytes"] = pair["bytes"]
                row["pair"] = pair["name"]
                row["pair_path"] = pair["rebuilt"]
                samples.append(row)
        result = {
            "pairs": pairs,
            "samples": samples,
            "closure": {
                "base_root": "/old", "rebuilt_root": "/new", "base_paths": 3, "rebuilt_paths": 3,
                "shared_paths": 1, "measured_pairs": 2, "rebuild_pairs": 1, "upgrade_pairs": 1,
                "measured_bytes": 400, "unpaired": [], "unpaired_bytes": 0,
            },
        }
        totals = benchmark.strategy_totals(result)
        self.assertEqual([total["strategy"] for total in totals], list(benchmark.STRATEGIES))
        self.assertEqual(totals[0]["physical_bytes"], 40)
        self.assertEqual(totals[0]["logical_bytes"], 400)
        report = benchmark.render_closure_report(result)
        self.assertIn("| exact/256KiB | 40.0 B | 10.000 |", report)
        self.assertIn("## rebuild (1 pairs, 100.0 B)", report)
        self.assertIn("| b | upgrade | 300.0 B | 10.00% |", report)


def closure_wire_row(phase: str, down: int, requests: int = 134) -> dict[str, object]:
    return {
        "schema": benchmark.CLOSURE_WIRE_SCHEMA,
        "phase": phase,
        "correctness": "passed",
        "pairs": 349,
        "rtt_ms": 50.0,
        "bandwidth_kib": 2560,
        "wall_seconds": 54.0,
        "logical_bytes": 1_500_000_000,
        "server_to_client_bytes": down,
        "client_to_server_bytes": 645180,
        "transport_requests": requests,
        "slice_copy_bytes": 0 if phase == "cold" else 903199983,
        "slice_literal_bytes": 1495493331 if phase == "cold" else 157547515,
    }


def wire_row(phase: str, down: int) -> dict[str, object]:
    return {
        "schema": benchmark.WIRE_SCHEMA,
        "phase": phase,
        "correctness": "passed",
        "rtt_ms": 50.0,
        "bandwidth_kib": 2560,
        "wall_seconds": 0.25,
        "server_to_client_bytes": down,
        "client_to_server_bytes": 700,
        "transport_requests": 7,
        "slice_copy_bytes": 0 if phase == "cold" else 746032,
        "slice_literal_bytes": 747292 if phase == "cold" else 1260,
    }


class WireTests(unittest.TestCase):
    def test_links_parse_and_reject_garbage(self):
        self.assertEqual(benchmark.parse_links("0:0, 12800:5,2560:50"), [(0, 0), (12800, 5), (2560, 50)])
        self.assertEqual(benchmark.parse_links("1024"), [(1024, 0)])
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_links("fast:slow")
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_links(",")

    def test_wire_rows_require_both_phases_and_passed_gates(self):
        rows = [wire_row("cold", 343311), wire_row("rebuild", 2148)]
        output = "noise\n" + "\n".join(json.dumps(row) for row in rows) + "\n"
        parsed = benchmark.parse_wire_rows(output)
        self.assertEqual([row["phase"] for row in parsed], ["cold", "rebuild"])
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_wire_rows(json.dumps(rows[0]) + "\n")
        failed = dict(rows[1], correctness="failed")
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_wire_rows("\n".join(json.dumps(row) for row in [rows[0], failed]) + "\n")

    def test_wire_table_names_the_link_and_phase(self):
        rows = [dict(wire_row("cold", 343311), pair="gmp"), dict(wire_row("rebuild", 2148), pair="gmp")]
        table = "\n".join(benchmark.render_wire_table(rows))
        self.assertIn("| gmp | 2560 KiB/s, 50 ms | cold | 335.3 KiB |", table)
        self.assertIn("| gmp | 2560 KiB/s, 50 ms | rebuild | 2.1 KiB |", table)
        self.assertEqual(benchmark.render_wire_table([]), [])

    def test_main_runs_the_wire_stub_per_link(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_tree(root / "base", {"lib/x.so": b"/nix/store/" + b"a" * 32 + b"-glibc"})
            write_tree(root / "rebuilt", {"lib/x.so": b"/nix/store/" + b"b" * 32 + b"-glibc"})
            rows = "\n".join(json.dumps(sample_row(strategy)) for strategy in benchmark.STRATEGIES)
            stub = root / "cdcs-stub"
            stub.write_text(f"#!/bin/sh\ncat >&2 <<'ROWS'\n{rows}\nROWS\n")
            stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
            wire_rows = "\n".join(json.dumps(wire_row(phase, 10)) for phase in ("cold", "rebuild"))
            wire = root / "wire-stub"
            wire.write_text(
                "#!/bin/sh\n"
                'test "$1" = "--base" -a "$3" = "--rebuilt" -a "$5" = "--bandwidth-kib" -a "$7" = "--rtt-ms" || exit 3\n'
                f"cat <<'ROWS'\n{wire_rows}\nROWS\n"
            )
            wire.chmod(wire.stat().st_mode | stat.S_IEXEC)
            output = root / "result.json"
            code = benchmark.main(
                [
                    "--base", str(root / "base"), "--rebuilt", str(root / "rebuilt"),
                    "--no-build", "--benchmark-bin", str(stub),
                    "--wire", "--wire-bin", str(wire), "--links", "0:0,2560:50",
                    "--output", str(output),
                ]
            )
            self.assertEqual(code, 0)
            result = json.loads(output.read_text())
            self.assertEqual(len(result["wire"]), 4)
            self.assertEqual(result["configuration"]["links"], "0:0,2560:50")
            self.assertIn("## Over the wire", output.with_suffix(".md").read_text())


class ClosureWireTests(unittest.TestCase):
    def rows(self, cold=525_696_865, rebuild=65_506_772, requests=134):
        return "\n".join(
            json.dumps(row)
            for row in (
                closure_wire_row("cold", cold),
                closure_wire_row("rebuild", rebuild, requests),
            )
        )

    def test_a_rebuild_must_cost_a_fraction_of_a_cold_sync(self):
        parsed = benchmark.parse_closure_wire_rows(self.rows(), 349)
        self.assertEqual([row["phase"] for row in parsed], ["cold", "rebuild"])
        with self.assertRaises(benchmark.CdcsBenchmarkError) as raised:
            benchmark.parse_closure_wire_rows(self.rows(rebuild=200_000_000), 349)
        self.assertIn("of the cold sync's bytes", str(raised.exception))

    def test_a_closure_must_keep_travelling_in_batches(self):
        with self.assertRaises(benchmark.CdcsBenchmarkError) as raised:
            benchmark.parse_closure_wire_rows(self.rows(requests=358), 349)
        self.assertIn("travelling in batches costs at most", str(raised.exception))

    def test_the_request_gate_leaves_room_for_a_sync_that_is_mostly_fixed_cost(self):
        # Three paths cost three requests, which is the handshake, the
        # discovery answer and the offer of bases rather than a per-path cost.
        benchmark.parse_closure_wire_rows(self.rows(rebuild=15245, requests=3), 3)

    def test_both_phases_must_verify_their_destination(self):
        failed = json.dumps(dict(closure_wire_row("rebuild", 10), correctness="failed"))
        output = json.dumps(closure_wire_row("cold", 100)) + "\n" + failed
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_closure_wire_rows(output, 349)
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_closure_wire_rows(json.dumps(closure_wire_row("cold", 100)), 349)

    def test_pairs_are_written_as_the_example_reads_them(self):
        with tempfile.TemporaryDirectory() as directory:
            path = benchmark.write_pairs_file(
                [{"name": "gmp-6.3.0", "base": "/nix/store/aaa-gmp", "rebuilt": "/nix/store/bbb-gmp"}],
                pathlib.Path(directory) / "pairs.txt",
            )
            self.assertEqual(path.read_text(), "gmp-6.3.0 /nix/store/aaa-gmp /nix/store/bbb-gmp\n")

    def test_table_names_the_link_and_counts_the_paths(self):
        table = "\n".join(
            benchmark.render_closure_wire_table(
                [closure_wire_row("cold", 525_696_865), closure_wire_row("rebuild", 65_506_772)]
            )
        )
        self.assertIn("| 2560 KiB/s, 50 ms | cold | 349 |", table)
        self.assertIn("| 2560 KiB/s, 50 ms | rebuild | 349 |", table)
        self.assertEqual(benchmark.render_closure_wire_table([]), [])

    def test_closure_wire_needs_a_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_tree(root / "base", {"lib/x.so": b"/nix/store/" + b"a" * 32 + b"-glibc"})
            write_tree(root / "rebuilt", {"lib/x.so": b"/nix/store/" + b"b" * 32 + b"-glibc"})
            stub = root / "cdcs-stub"
            rows = "\n".join(json.dumps(sample_row(strategy)) for strategy in benchmark.STRATEGIES)
            stub.write_text(f"#!/bin/sh\ncat >&2 <<'ROWS'\n{rows}\nROWS\n")
            stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
            errors = io.StringIO()
            with contextlib.redirect_stderr(errors):
                code = benchmark.main(
                    [
                        "--base", str(root / "base"), "--rebuilt", str(root / "rebuilt"),
                        "--no-build", "--benchmark-bin", str(stub),
                        "--closure-wire", "--closure-wire-bin", str(stub),
                        "--output", str(root / "result.json"),
                    ]
                )
            self.assertEqual(code, 2)
            self.assertIn("--closure-base", errors.getvalue())


class SampleParsingTests(unittest.TestCase):
    def test_parser_requires_every_strategy_to_pass_its_gate(self):
        rows = [sample_row(strategy) for strategy in benchmark.STRATEGIES]
        output = "noise\n" + "\n".join(json.dumps(row) for row in rows) + "\n"
        parsed = benchmark.parse_samples(output)
        self.assertEqual([row["strategy"] for row in parsed], list(benchmark.STRATEGIES))

        failed = dict(rows[0], correctness="failed")
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_samples(json.dumps(failed) + "\n")
        with self.assertRaises(benchmark.CdcsBenchmarkError):
            benchmark.parse_samples("\n".join(json.dumps(row) for row in rows[:-1]) + "\n")

    def test_benchmark_artifact_parser_finds_the_bench_binary(self):
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": "cdcs", "kind": ["bench"]},
            "executable": "/tmp/target/release/deps/cdcs-1234",
        }
        self.assertEqual(
            benchmark.parse_benchmark_binary(json.dumps(artifact)),
            pathlib.Path("/tmp/target/release/deps/cdcs-1234"),
        )


class EndToEndTests(unittest.TestCase):
    def test_main_runs_a_stub_binary_and_writes_result_and_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_tree(root / "base", {"lib/x.so": b"/nix/store/" + b"a" * 32 + b"-glibc"})
            write_tree(root / "rebuilt", {"lib/x.so": b"/nix/store/" + b"b" * 32 + b"-glibc"})
            rows = "\n".join(json.dumps(sample_row(strategy)) for strategy in benchmark.STRATEGIES)
            stub = root / "cdcs-stub"
            stub.write_text(
                "#!/bin/sh\n"
                'test "$1" = "--test" || exit 3\n'
                'test -n "$CASITA_CDCS_BASE" || exit 4\n'
                'test -n "$CASITA_CDCS_REBUILT" || exit 5\n'
                f"cat >&2 <<'ROWS'\n{rows}\nROWS\n"
            )
            stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
            output = root / "result.json"
            code = benchmark.main(
                [
                    "--base",
                    str(root / "base"),
                    "--rebuilt",
                    str(root / "rebuilt"),
                    "--no-build",
                    "--benchmark-bin",
                    str(stub),
                    "--output",
                    str(output),
                ]
            )
            self.assertEqual(code, 0)
            result = json.loads(output.read_text())
            self.assertEqual(result["result_schema"], benchmark.RESULT_SCHEMA)
            self.assertEqual(len(result["samples"]), len(benchmark.STRATEGIES))
            self.assertEqual(result["pairs"][0]["files"], 1)
            self.assertTrue(all(sample["pair"] == "base" for sample in result["samples"]))
            report = output.with_suffix(".md").read_text()
            self.assertIn("## base", report)
            self.assertIn("| slices/1KiB/16 | 1 |", report)
            self.assertNotIn("CASITA_CDCS_REPORT", os.environ)


if __name__ == "__main__":
    unittest.main()
