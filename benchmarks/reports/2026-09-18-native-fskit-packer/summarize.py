"""Validate the retained profile and print the packer's sampled call subtree."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent

if __name__ == "__main__":
    report = json.loads((ROOT / "packer-profile.json").read_text())
    assert report["complete"] and report["comparison_complete"]
    assert not report["cleanup_errors"]
    sample = report["extension_sample"]
    assert sample["status"] == "passed" and sample["returncode"] == 0
    assert sample["pid"] == report["native_process"]["pid"]
    stacks = (ROOT / Path(sample["stacks"]).name).read_text()
    assert f"(pid {sample['pid']})" in stacks
    assert "org.casita.native-fskit.extension.repository" in stacks
    marker = "objc2_fs_kit::generated::__FSVolume::FSDirectoryEntryPacker::packEntryWithName"
    lines = stacks.splitlines()
    start = next(i for i, line in enumerate(lines) if marker in line)
    # Print a bounded excerpt; sample counts are inclusive within a call tree.
    print("Native extension PID and sample identity match. All benchmark gates passed.")
    print("\n".join(lines[start:start + 30]))
    print("\nDo not add parent and child sample counts or treat idle-thread samples as CPU time.")
