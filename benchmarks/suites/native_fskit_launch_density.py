"""Launch/path-resolution scaling with sibling count; reuse the exact compiled bundle."""
import argparse
import json
from pathlib import Path

from benchmarks.suites import native_fskit_launch as launch
from benchmarks.suites import repository as common
from benchmarks.suites.filesystem_transports import require

COUNTS = (0, 128, 256, 512)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--server-binary", type=Path)
    parser.add_argument("--enumeration-attributes", choices=("requested", "eager"), default="requested")
    parser.add_argument("--enumeration-cache", choices=("enabled", "disabled"), default="enabled")
    parser.add_argument("--enumeration-timing", choices=("basic", "detailed"), default="basic")
    parser.add_argument("--filename-construction", choices=("data", "bytes"), default="data")
    parser.add_argument("--volume-capabilities", choices=("minimal", "explicit"), default="minimal")
    parser.add_argument("--item-timestamps", choices=("zero", "store"), default="store")
    parser.add_argument("--reader-cache", choices=("enabled", "disabled"), default="enabled")
    parser.add_argument("--xattr-mode", choices=("explicit", "emulated"), default="emulated")
    args = parser.parse_args(argv)
    output = args.output.resolve()
    result = {"result_schema":"casita.native-fskit-launch-density.v1", "complete":False,
              "decision_eligible":False, "metadata_counts":COUNTS, "runs":[]}
    bundle, server = args.bundle, args.server_binary
    try:
        for count in COUNTS:
            child = output.parent / (output.stem + f"-{count}.json")
            arguments = ["--metadata-files", str(count), "--output", str(child), "--profile", args.profile,
                         "--repetitions", str(args.repetitions), "--enumeration-attributes", args.enumeration_attributes,
                         "--xattr-mode", args.xattr_mode, "--enumeration-cache", args.enumeration_cache,
                         "--enumeration-timing", args.enumeration_timing,
                         "--filename-construction", args.filename_construction,
                         "--volume-capabilities", args.volume_capabilities,
                         "--item-timestamps", args.item_timestamps, "--reader-cache", args.reader_cache]
            if bundle:
                require(server is not None, "bundle reuse requires matching server")
                arguments += ["--bundle", str(bundle), "--server-binary", str(server)]
            code = launch.main(arguments)
            report = json.loads(child.read_text())
            result["runs"].append({"metadata_files":count, "report":str(child), "complete":report["complete"]})
            require(code == 0 and report["complete"] and report["comparison_complete"], "density case failed")
            identity = (report["binaries"], report["server_sha256"], report["build_identity"], report["source_sha256"])
            if count == COUNTS[0]:
                reference = identity
            else:
                require(identity == reference, "density cases used different builds")
            bundle, server = report["bundle"], report["server_binary"]
            common.write_atomic(output, json.dumps(result, indent=2)+"\n")
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
    finally:
        common.write_atomic(output, json.dumps(result, indent=2)+"\n")
    return 0 if result["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
