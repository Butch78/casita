"""Rootless native Rust FSKit installation probe and mounted transport benchmark."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import mmap
import os
from pathlib import Path
import platform
import plistlib
import shutil
import stat
import subprocess
import tempfile
import time

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.filesystem_transports import require

SOURCE = cli.ROOT / "crates/casita-fskit"
SIZES = (0, 1, 4095, 4096, 4097, 16383, 16384, 16385, 65535, 65536, 65537,
         131071, 131072, 131073, 1048575, 1048576, 1048577)
IDENTIFIER = "org.casita.native-fskit.extension"
FIXTURE_ID = "casita-memory-filesystem-v1"
PORTABLE_FIXTURE_ID = "casita-memory-filesystem-portable-v1"
NATIVE = "native-rust-fskit"
HOST = "host"
LSREGISTER = "/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister"


def fixture(portable=False):
    files = {f"size-{size}": bytes((i * 31 + size) % 251 for i in range(size)) for size in SIZES}
    files.update({f"meta-{i:04}": f"metadata-{i}\n".encode() for i in range(256)})
    files.update({"run": b"#!/bin/sh\nprintf 'casita-native-fskit-ok\\n'\n",
                  "byte-ascii" if portable else os.fsdecode(b"byte-\xff"): b"byte-safe\n"})
    return files


def materialize(root, files):
    root.mkdir()
    for name, data in files.items():
        (root / name).write_bytes(data)
        (root / name).chmod(0o555 if name == "run" else 0o444)
    (root / "link").symlink_to("size-4096")


def check_tree(root, files):
    actual, expected = set(os.listdir(root)), set(files) | {"link"}
    require(actual == expected, f"directory enumeration differs: missing={expected - actual!r}, extra={actual - expected!r}")
    for name, expected in files.items():
        path = root / name
        require(path.read_bytes() == expected, f"read differs: {name!r}")
        require(path.stat().st_size == len(expected), f"size differs: {name!r}")
        with path.open("rb") as handle:
            for offset in (0, max(0, len(expected) - 1), len(expected), len(expected) + 1):
                require(os.pread(handle.fileno(), 17, offset) == expected[offset:offset + 17], "pread/EOF differs")
            if expected:
                with mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
                    require(mapped[:] == expected, "mmap differs")
    require(os.readlink(root / "link") == "size-4096", "symlink target differs")
    require((root / "link").read_bytes() == files["size-4096"], "symlink read differs")
    require(bool((root / "run").stat().st_mode & stat.S_IXUSR), "execute bit missing")
    completed = subprocess.run([str(root / "run")], capture_output=True, timeout=15, check=True)
    require(completed.stdout == b"casita-native-fskit-ok\n", "script execution differs")


class NativeRun:
    def __init__(self, work, result):
        self.work, self.result = work, result
        self.devices, self.mounts = [], []
        self.portable = result.get("configuration", {}).get("portable_names", False)
        self.repository_mode = result.get("configuration", {}).get("repository", False)
        self.identifier = IDENTIFIER + ".repository" if self.repository_mode else IDENTIFIER

    def command(self, args, timeout=60, env=None):
        started = time.perf_counter_ns()
        record = {"argv": list(map(str, args)), "uid": os.getuid()}
        self.result["commands"].append(record)
        try:
            process = subprocess.run(record["argv"], capture_output=True, timeout=timeout, env=env)
            record.update(returncode=process.returncode, elapsed_ns=time.perf_counter_ns() - started,
                          stdout=process.stdout.decode(errors="replace"), stderr=process.stderr.decode(errors="replace"))
            require(process.returncode == 0, f"command failed: {args[0]}: {record['stderr'][-2000:]}")
            return process.stdout
        except subprocess.TimeoutExpired:
            record.update(status="timeout", elapsed_ns=time.perf_counter_ns() - started)
            raise

    def build_identity(self):
        sdk_root = os.environ.get("SDKROOT")
        if sdk_root:
            settings = Path(sdk_root) / "SDKSettings.json"
            require(settings.is_file(), "SDKROOT must point to an Apple SDK containing SDKSettings.json")
            sdk = {"root": str(Path(sdk_root).resolve()),
                   "version": json.loads(settings.read_text())["Version"],
                   "settings_sha256": hashlib.sha256(settings.read_bytes()).hexdigest()}
        else:
            sdk = {"version": self.command(["xcrun", "--show-sdk-version"]).decode().strip()}
        return {"rustc": self.command(["rustc", "-vV"]).decode(),
                "cargo": self.command(["cargo", "--version"]).decode(),
                "sdk": sdk,
                "architecture": platform.machine(), "profile": "release",
                "portable_names": self.portable,
                "repository": self.repository_mode,
                "deployment_target": "15.4",
                "build_overrides": {key: value for key, value in os.environ.items()
                    if key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET", "CC", "SDKROOT"}
                    or (key.startswith("CARGO_TARGET_") and key.endswith("_LINKER"))
                    or key.startswith("CARGO_PROFILE_RELEASE_")}}

    def prepare(self, identity):
        target = Path(os.environ.get("CASITA_NATIVE_TARGET_DIR", self.work / "target")).resolve()
        self.result["build_target_directory"] = str(target)
        self.result["build_identity"] = self.build_identity()
        environment = {**os.environ, "MACOSX_DEPLOYMENT_TARGET": "15.4"}
        features = ["--features", "portable-names"] if self.portable else []
        if self.repository_mode:
            features = ["--features", "repository"]
        self.command(["cargo", "build", "--locked", "--release", *features, "--manifest-path", SOURCE / "Cargo.toml",
                      "--target-dir", target], timeout=600, env=environment)
        app = self.work / "CasitaNativeFSKit.app"
        extension = app / "Contents/Extensions/casita-native-fskit-extension.appex"
        for source, bundle, binary in (("extension", extension, "casita-native-fskit-extension"),
                                       ("host", app, "casita-native-fskit")):
            (bundle / "Contents/MacOS").mkdir(parents=True, exist_ok=True)
            shutil.copy2(target / "release" / binary, bundle / "Contents/MacOS" / binary)
            shutil.copy2(SOURCE / source / "Info.plist", bundle / "Contents/Info.plist")
            if self.repository_mode:
                info_path = bundle / "Contents/Info.plist"
                info = plistlib.loads(info_path.read_bytes())
                info["CFBundleIdentifier"] = self.identifier if source == "extension" else "org.casita.native-fskit.repository-host"
                info["LSMinimumSystemVersion"] = "26.0"
                if source == "extension":
                    attributes = info["EXAppExtensionAttributes"]
                    attributes.update(FSShortName="casitarepo", FSSupportsBlockResources=False,
                                      FSSupportsPathURLs=True, FSRequiresSecurityScopedPathURLResources=True)
                info_path.write_bytes(plistlib.dumps(info))
            if source == "host":
                resources = bundle / "Contents/Resources"
                resources.mkdir()
                (resources / "source.json").write_text(json.dumps(self.result["source_sha256"], indent=2) + "\n")
                (resources / "build.json").write_text(json.dumps(self.result["build_identity"], indent=2) + "\n")
            entitlements = "adhoc.entitlements" if source == "extension" and identity == "-" else "main.entitlements"
            self.command(["codesign", "--force", "--sign", identity, "--timestamp=none", "--options", "runtime",
                          "--entitlements", SOURCE / source / entitlements, "--generate-entitlement-der", bundle])
            self.command(["codesign", "--verify", "--strict", bundle])
            self.result.setdefault("binaries", {})[binary] = hashlib.sha256((bundle / "Contents/MacOS" / binary).read_bytes()).hexdigest()
        self.register(app)
        self.result["bundle"] = str(app)
        self.result["signing"] = "ad-hoc development only" if identity == "-" else "explicit signing identity"
        return app

    def register(self, app):
        target = Path(os.environ.get("CASITA_NATIVE_TARGET_DIR", self.work / "target")).resolve()
        features = ["--features", "repository"] if self.repository_mode else (
            ["--features", "portable-names"] if self.portable else [])
        self.command(["cargo", "build", "--locked", "--release", *features,
                      "--manifest-path", SOURCE / "Cargo.toml", "--target-dir", target,
                      "--bin", "fskit-native-setup"], timeout=600,
                     env={**os.environ, "MACOSX_DEPLOYMENT_TARGET": "15.4"})
        self.command([target / "release/fskit-native-setup", app, self.identifier])

    def mount_command(self, args):
        deadline = time.monotonic() + 15
        while True:
            try:
                return self.command(args)
            except common.BenchmarkError:
                last = self.result["commands"][-1]
                message = last.get("stderr", "")
                startup = ("communicate with a helper application" in message
                           or "com.apple.extensionKit.errorDomain error 2" in message)
                if (last.get("returncode") != 69 or "Unable to invoke task" not in message or not startup
                        or os.path.ismount(args[-1]) or time.monotonic() >= deadline):
                    raise
                time.sleep(0.25)

    def mount(self):
        index = len(self.devices)
        image = self.work / f"resource-{index}.img"
        with image.open("xb") as handle:
            handle.truncate(16 * 1024**2)
        attached = plistlib.loads(self.command(["hdiutil", "attach", "-plist", "-nomount", "-imagekey",
                                                "diskimage-class=CRawDiskImage", image]))
        devices = [entry["dev-entry"] for entry in attached["system-entities"] if entry.get("dev-entry")]
        self.devices.extend(devices)
        require(len(devices) == 1, f"expected one private raw device, got {devices}")
        root = self.work / f"mount-{index}"
        root.mkdir()
        # Track before mounting so cleanup also covers a partially successful command.
        self.mounts.append(root)
        self.mount_command(["/sbin/mount", "-F", "-t", "casitanative", devices[0], root])
        require(os.path.ismount(root), "mount command did not create a mounted filesystem")
        self.result["rootless_mount"] = True
        return root

    def unmount(self, root):
        self.command(["/sbin/umount", root])
        require(not os.path.ismount(root), "unmount left filesystem attached")
        self.mounts.remove(root)

    def cleanup(self):
        errors = []
        for root in self.mounts[:]:
            try:
                if os.path.ismount(root):
                    self.unmount(root)
                else:
                    self.mounts.remove(root)
            except Exception as error:
                errors.append(str(error))
        for device in self.devices[:]:
            try:
                # Never force-detach busy devices; preserve diagnostics and report failure.
                self.command(["hdiutil", "detach", device])
                self.devices.remove(device)
            except Exception as error:
                errors.append(str(error))
        self.result["cleanup_errors"] = errors
        return not errors


def measurements(root, files, implementation, repetition, iterations):
    rows = []
    def sample(case, operation, count=iterations, **dimensions):
        latencies = []
        for _ in range(count):
            start = time.perf_counter_ns()
            operation()
            latencies.append(time.perf_counter_ns() - start)
        ordered = sorted(latencies)
        rows.append(dict(case=case, implementation=implementation, repetition=repetition,
                         elapsed_ns=latencies, p50_ns=ordered[len(ordered)//2],
                         p95_ns=ordered[min(len(ordered)-1, int(len(ordered)*.95))],
                         correctness="passed", **dimensions))
    def listing():
        require(set(os.listdir(root)) == set(files) | {"link"}, "listing differs")
    sample("readdir", listing)
    def metadata():
        for i in range(256):
            name = f"meta-{i:04}"
            require((root / name).stat().st_size == len(files[name]), "stat differs")
    sample("stat-256", metadata)
    for size in SIZES:
        name = f"size-{size}"
        expected = files[name]
        def read():
            require((root / name).read_bytes() == expected, "read differs")
        sample("open-read-close", read, size=size)
        with (root / name).open("rb") as handle:
            def held():
                require(os.pread(handle.fileno(), size + 1, 0) == expected, "held read differs")
            sample("held-fd-read", held, size=size)
    for workers in (1, 4, 16):
        with ThreadPoolExecutor(max_workers=workers) as pool:
            def parallel():
                values = list(pool.map(lambda _: (root / "size-65537").read_bytes(), range(32)))
                require(all(data == files["size-65537"] for data in values), "concurrent reads differ")
            sample("parallel-read-32", parallel, workers=workers)
    return rows


def paired_comparisons(samples, repetitions, fixture_id=FIXTURE_ID, extra_dimensions=()):
    """Compare native FSKit with the ordinary host filesystem."""
    def key(row):
        return row["repetition"], row["case"], row.get("size"), row.get("workers")
    groups = {}
    for row in samples:
        if row["implementation"] not in (NATIVE, HOST):
            continue
        pair = groups.setdefault(key(row), {})
        require(row["implementation"] not in pair, "duplicate controlled sample")
        require(row["correctness"] == "passed", "controlled sample failed correctness")
        pair[row["implementation"]] = row
    expected = {(repetition, case, size, workers)
                for repetition in range(repetitions)
                for case, size, workers in [("readdir", None, None), ("stat-256", None, None)]
                + [(case, size, None) for size in SIZES for case in ("open-read-close", "held-fd-read")]
                + [("parallel-read-32", None, workers) for workers in (1, 4, 16)] + list(extra_dimensions)}
    require(set(groups) == expected, "controlled comparison has missing or unexpected cases")
    rows = []
    for dimensions, pair in groups.items():
        require(set(pair) == {NATIVE, HOST}, "each controlled case needs native FSKit and the host")
        native, host = pair[NATIVE], pair[HOST]
        require(len(native["elapsed_ns"]) == len(host["elapsed_ns"]), "paired iteration counts differ")
        require(native["p50_ns"] > 0 and native["p95_ns"] > 0, "invalid native latency")
        rows.append(dict(repetition=dimensions[0], case=dimensions[1], size=dimensions[2], workers=dimensions[3],
                         fixture=fixture_id, p50_host_over_native=host["p50_ns"] / native["p50_ns"],
                         p95_host_over_native=host["p95_ns"] / native["p95_ns"]))
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--portable-names", action="store_true", help="use identical ASCII names for native and host; does not pass the raw-byte compatibility gate")
    parser.add_argument("--host-only", action="store_true", help="validate the harness; never a native performance result")
    parser.add_argument("--bundle", type=Path, help="reuse a previously prepared and enabled probe app")
    parser.add_argument("--identity", default="-", help="codesign identity; default is local ad-hoc development")
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.host_only and (args.prepare_only or args.bundle)):
        parser.error("positive repetitions required; host-only cannot mount or prepare extensions")
    args.output = args.output.resolve()
    fixture_id = PORTABLE_FIXTURE_ID if args.portable_names else FIXTURE_ID
    args.output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="casita-native-fskit-"))
    result = dict(schema_version=3, suite_id="repository-e2e", result_schema="casita.native-fskit.v3",
                  complete=False, comparison_complete=False, decision_eligible=False, rootless_mount=False,
                  environment={"platform": platform.platform(), "macos": platform.mac_ver()[0],
                               "uid": os.getuid(), "effective_uid": os.geteuid()},
                  work_directory=str(work), configuration={"profile": args.profile, "repetitions": args.repetitions,
                  "host_only": args.host_only, "comparison": "native-host", "fixture": fixture_id, "sizes": SIZES,
                  "portable_names": args.portable_names,
                  "cache_policy": "warm correctness pass before timing; no cold-cache claim",
                  "validation_in_timing": True, "native_storage": "in-memory synthetic fixture; no Casita engine",
                  "order": "implementation order rotates each repetition; order recorded per round",
                  "adapter_policies": {NATIVE: "FSKit-managed caching and callback concurrency; borrowed read buffer",
                                       HOST: "ordinary host filesystem reads"}},
                  commands=[], samples=[], gates={}, rounds=[], comparisons=[])
    result["source_sha256"] = {str(path.relative_to(cli.ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in [Path(__file__), *sorted((SOURCE / "src").rglob("*.rs")),
                     *sorted((SOURCE / "extension").rglob("*.rs")), *sorted((SOURCE / "host").rglob("*.rs")), *sorted((SOURCE / "setup").rglob("*.rs")),
                      SOURCE / "build.rs", SOURCE / "Cargo.toml", SOURCE / "Cargo.lock",
                     cli.ROOT / "crates/fskit-native/Cargo.toml", *sorted((cli.ROOT / "crates/fskit-native/src").rglob("*.rs")),
                     *sorted(SOURCE.rglob("*.plist")), *sorted(SOURCE.rglob("*.entitlements"))]}
    native = NativeRun(work, result)
    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    save()
    try:
        require(os.getuid() != 0 and os.geteuid() != 0, "run as an ordinary user, not root")
        if not args.host_only:
            require(platform.system() == "Darwin", "native FSKit requires a macOS 15.4+ test host")
            version = tuple(map(int, platform.mac_ver()[0].split(".")[:2]))
            require(version >= (15, 4), "FSKit requires macOS 15.4+")
            if args.bundle:
                app = args.bundle.resolve()
                native.command(["codesign", "--verify", "--deep", "--strict", app])
                built_sources = json.loads((app / "Contents/Resources/source.json").read_text())
                require(built_sources == result["source_sha256"], "bundle sources differ; prepare the current prototype again")
                result["build_identity"] = native.build_identity()
                require(json.loads((app / "Contents/Resources/build.json").read_text()) == result["build_identity"],
                        "bundle compiler, SDK or build overrides differ; prepare it again")
                result["bundle"] = str(app)
                result["binaries"] = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in app.rglob("*") if path.is_file() and path.parent.name == "MacOS"}
                native.register(app)
            else:
                native.prepare(args.identity)
            if args.prepare_only:
                result["status"] = "prepared and activated through Rust setup"
                return 0
        files = fixture(args.portable_names)
        # APFS rejects invalid UTF-8 names. Keep that gate for the adapters,
        # but explicitly exclude it from the host/production-source control.
        host_files = {name: data for name, data in files.items()
                      if platform.system() != "Darwin" or os.fsencode(name) != b"byte-\xff"}
        result["host_fixture_exclusions"] = sorted(set(files) - set(host_files))
        materialize(work / "host", host_files)
        implementations = [("host", work / "host")]
        if not args.host_only:
            root = native.mount()
            implementations.append((NATIVE, root))
            second = native.mount()
            check_tree(second, files)
            native.unmount(second)
            check_tree(root, files)
            result["gates"]["second_mount_unmount_preserves_first"] = "passed"
        for name, root in implementations:
            expected = files if name == NATIVE else host_files
            check_tree(root, expected)
            result["gates"][name] = {"correctness": "passed", "byte_names": os.fsdecode(b"byte-\xff") in expected}
        iterations = 3 if args.profile == "smoke" else 30
        for repetition in range(args.repetitions):
            offset = repetition % len(implementations)
            ordered = implementations[offset:] + implementations[:offset]
            result["rounds"].append({"repetition": repetition, "order": [name for name, _ in ordered]})
            for name, root in ordered:
                expected = files if name == NATIVE else host_files
                result["samples"].extend(measurements(root, expected, name, repetition, iterations))
                save()
        if not args.host_only:
            result["comparisons"] = paired_comparisons(result["samples"], args.repetitions, fixture_id)
            result["comparison_complete"] = True
        result.update(complete=True, status="host control only" if args.host_only else "native prototype measured")
    except Exception as error:
        result.update(status="failed", error=str(error), complete=False)
    finally:
        if not native.cleanup():
            result["complete"] = False
        if not result["complete"]:
            result["comparison_complete"] = False
        save()
        print(f"native-fskit: {result.get('status')}; {args.output}", flush=True)
        if result.get("error"):
            print(result["error"], flush=True)
    return 0 if result["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
