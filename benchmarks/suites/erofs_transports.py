"""Benchmark kernel EROFS against Casita FUSE and ext4 in a disposable KVM guest."""
import argparse
import base64
import gzip
import json
import lzma
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

from benchmarks import cli
from benchmarks.suites import filesystem_transports as fs


def tool(name, pattern):
    found = shutil.which(name)
    candidates = sorted(Path("/nix/store").glob(pattern), reverse=True)
    if found:
        return str(Path(found).resolve())
    fs.require(candidates, f"{name} missing; install it or specify its tool option")
    return str(candidates[0])


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--profile", choices=["smoke", "standard"], default="smoke")
    p.add_argument("--repetitions", type=int, default=3)
    p.add_argument("--corpora", default="random,compressible")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--server-binary", type=Path)
    p.add_argument("--kernel", type=Path, default=Path("/run/booted-system/kernel"))
    p.add_argument("--modules", type=Path)
    p.add_argument("--qemu")
    p.add_argument("--busybox")
    p.add_argument("--mkfs-erofs")
    p.add_argument("--mkfs-ext4")
    p.add_argument("--cpio")
    p.add_argument("--vcpus", type=int, default=4)
    p.add_argument("--memory-mib", type=int, default=2048)
    p.add_argument("--timeout", type=int, default=900)
    p.add_argument("--measurement-note", default="")
    return p


def copy_absolute(source, root):
    destination = root / source.relative_to("/")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if source.is_dir():
        shutil.copytree(source, destination, symlinks=True, dirs_exist_ok=True)
    else:
        shutil.copy2(source, destination)


def module_commands(modules, root):
    deps = dict(line.split(":", 1) for line in (modules / "modules.dep").read_text().splitlines())
    order = []
    def add(name):
        if name not in order:
            for dependency in deps[name].split():
                add(dependency)
            order.append(name)
    for wanted in ("virtio_pci", "virtio_blk", "ext4", "erofs", "fuse"):
        matches = [name for name in deps if Path(name).name.split(".ko")[0] == wanted]
        if matches:
            add(matches[0])
    commands = []
    for index, name in enumerate(order):
        data = (modules / name).read_bytes()
        if name.endswith(".xz"):
            data = lzma.decompress(data)
        else:
            fs.require(name.endswith(".ko"), "unsupported module compression")
        (root / f"modules/{index}.ko").write_bytes(data)
        commands.append(f"/bin/busybox insmod /modules/{index}.ko")
    return commands


def main(argv=None):
    args = parser().parse_args(argv)
    fs.require(min(args.repetitions, args.vcpus, args.memory_mib, args.timeout) > 0, "arguments must be positive")
    corpora = args.corpora.split(",")
    fs.require(corpora and set(corpora) <= {"random", "compressible"} and len(set(corpora)) == len(corpora), "invalid corpora")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="casita-erofs-comparison-"))
    result = {"schema_version": 1, "suite_id": "repository-e2e", "result_schema": "casita.erofs-transports.v1",
              "complete": False, "decision_eligible": False, "work_directory": str(work),
              "host_environment": fs.common.environment_metadata(work), "measurement_note": args.measurement_note}
    save = lambda: fs.common.write_atomic(output, json.dumps(result, indent=2) + "\n")
    save()
    try:
        fs.require(os.access("/dev/kvm", os.R_OK | os.W_OK), "accessible KVM required; no emulation timing fallback")
        qemu = args.qemu or tool("qemu-system-x86_64", "*qemu-*/bin/qemu-system-x86_64")
        busybox = args.busybox or tool("busybox-static", "*busybox-static-*/bin/busybox")
        mkfs = args.mkfs_erofs or tool("mkfs.erofs", "*erofs-utils-*/bin/mkfs.erofs")
        ext4 = args.mkfs_ext4 or tool("mkfs.ext4", "*e2fsprogs-*/bin/mkfs.ext4")
        cpio = args.cpio or tool("cpio", "*cpio-*/bin/cpio")
        bwrap = tool("bwrap", "*bubblewrap-*/bin/bwrap")
        modules = args.modules
        if modules is None:
            choices = list(Path("/run/booted-system/kernel-modules/lib/modules").glob("*/modules.dep"))
            fs.require(len(choices) == 1, "specify matching --modules directory")
            modules = choices[0].parent
        binary = (args.server_binary or fs.build_server(output)).resolve()
        result["artifacts"] = {name: {"path": str(path), "sha256": fs.digest(Path(path).read_bytes())}
            for name, path in {"server": binary, "kernel": args.kernel.resolve(), "mkfs": mkfs,
                               "qemu": qemu, "busybox": busybox}.items()}
        result["mkfs_version"] = subprocess.check_output([mkfs, "--version"], text=True).strip()
        result["source_sha256"] = {str(p.relative_to(cli.ROOT)): fs.digest(p.read_bytes())
            for p in [cli.ROOT / "Cargo.toml", cli.ROOT / "crates/casita/Cargo.toml", cli.ROOT / "Cargo.lock",
                      *sorted((cli.ROOT / "crates/casita-fs").rglob("*.rs")),
                      *sorted((cli.ROOT / "crates/casita/src").rglob("*.rs")),
                      *[cli.ROOT / f"benchmarks/suites/{name}.py" for name in ("filesystem_transports", "erofs_transports", "erofs_guest")]]}
        root = work / "root"
        for name in ("bin", "dev", "proc", "sys", "cache", "tmp", "modules", "repo", "etc"):
            (root / name).mkdir(parents=True)
        shutil.copy2(busybox, root / "bin/busybox")
        for name in ("sh", "echo", "mount", "umount", "findmnt"):
            (root / f"bin/{name}").symlink_to("busybox")
        shutil.copy2(binary, root / "transport_server")
        # Copy runtime closures without exposing the host filesystem to the VM.
        # This launcher targets the repository's Nix development environment.
        python = Path(sys.executable).resolve()
        prefixes = []
        for executable in (python, Path(mkfs), Path(bwrap)):
            fs.require(str(executable).startswith("/nix/store/"), "VM launcher currently requires Nix tool closures")
            prefixes.append(str(Path(*executable.parts[:4])))
        closure = subprocess.check_output(["nix-store", "-qR", *prefixes], text=True).splitlines()
        for path in closure:
            copy_absolute(Path(path), root)
        result["runtime_closure"] = closure
        linkage = subprocess.check_output(["ldd", str(binary)], text=True)
        for path in set(re.findall(r"/[^\s()]+", linkage)):
            copy_absolute(Path(path), root)
        for path in (cli.ROOT / "benchmarks").rglob("*.py"):
            relative = path.relative_to(cli.ROOT)
            (root / "repo" / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, root / "repo" / relative)
        commands = module_commands(modules, root)
        config = {"profile": args.profile, "repetitions": args.repetitions, "corpora": corpora, "mkfs": mkfs,
                  "vcpus": args.vcpus, "memory_mib": args.memory_mib,
                  "cache_policy": "first/repeat; no eviction; preparation and prior operations warm storage",
                  "disk": "fresh ext4 on virtio block; raw host file; cache=none", "compression": ["none", "lz4"]}
        (root / "config.json").write_text(json.dumps(config))
        result["configuration"] = config
        init = "#!/bin/sh\nexport PATH=/bin:" + shlex.quote(str(Path(bwrap).parent)) + "\n"
        init += "trap 'echo CASITA_EROFS_VM_FAILED; /bin/busybox poweroff -f' EXIT\nset -eu\n"
        init += "/bin/busybox mount -t proc proc /proc\n/bin/busybox mount -t sysfs sysfs /sys\n/bin/busybox mount -t devtmpfs devtmpfs /dev\n"
        init += "\n".join(commands) + "\n/bin/busybox mount -t ext4 /dev/vda /cache\n"
        init += "cd /repo\n" + shlex.quote(str(python)) + " -m benchmarks.suites.erofs_guest /config.json\n"
        init += "/bin/busybox umount /cache\necho CASITA_EROFS_VM_OK\ntrap - EXIT\n/bin/busybox poweroff -f\n"
        (root / "init").write_text(init)
        (root / "init").chmod(0o755)
        names = ["."] + [str(p.relative_to(root)) for p in sorted(root.rglob("*"))]
        packed = subprocess.run([cpio, "--quiet", "-o", "-H", "newc", "--null"], cwd=root,
            input="\0".join(names).encode() + b"\0", stdout=subprocess.PIPE, check=True).stdout
        initrd = work / "initrd.gz"
        initrd.write_bytes(gzip.compress(packed, compresslevel=1, mtime=0))
        disk = work / "benchmark.ext4"
        with disk.open("wb") as stream:
            stream.truncate(4 * 1024**3)
        subprocess.run([ext4, "-q", "-F", str(disk)], check=True)
        command = [qemu, "-enable-kvm", "-cpu", "host", "-m", str(args.memory_mib), "-smp", str(args.vcpus),
                   "-nographic", "-no-reboot", "-nic", "none", "-kernel", str(args.kernel.resolve()), "-initrd", str(initrd),
                   "-append", "console=ttyS0 panic=-1 quiet", "-drive", f"file={disk},format=raw,if=virtio,cache=none"]
        result["command"] = command
        result["host_load_before"] = os.getloadavg()
        save()
        print(f"Kernel EROFS comparison in KVM; console: {work / 'console.log'}", flush=True)
        with (work / "console.log").open("w") as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=args.timeout)
        result["host_load_after"] = os.getloadavg()
        console = (work / "console.log").read_text()
        match = re.search(r"CASITA_EROFS_RESULT=([A-Za-z0-9+/=]+)", console)
        fs.require(match, "VM produced no results; inspect console.log")
        result["guest"] = json.loads(gzip.decompress(base64.b64decode(match[1])))
        fs.require("CASITA_EROFS_VM_OK" in console and "CASITA_EROFS_VM_FAILED" not in console and result["guest"]["complete"],
                   "guest benchmark failed; inspect guest error and console.log")
        result["complete"] = True
        result["security_complete"] = result["guest"]["security_complete"]
    except BaseException as error:
        result["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        save()
    print(f"Results: {output}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
