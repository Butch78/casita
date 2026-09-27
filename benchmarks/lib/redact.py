"""Keep recorded results free of one machine's directory layout.

A result file is read long after the run that produced it, often from another
checkout. An absolute path in it says where one maintainer happened to put a
binary or an output file, which tells a later reader nothing and ties the
result to a session rather than to a run. Store paths are the exception: they
name their own content, so they stay.
"""
from __future__ import annotations

from typing import Any

PLACEHOLDER = "<local>"
KEEP_PREFIXES = ("/nix/store/",)


def local_path(value: str) -> str:
    """Replace the directory of an absolute path, keeping its final name."""
    if not value.startswith("/") or value.startswith(KEEP_PREFIXES):
        return value
    name = value.rstrip("/").rsplit("/", 1)[-1]
    return f"{PLACEHOLDER}/{name}" if name else PLACEHOLDER


def local_paths(value: Any) -> Any:
    """Rewrite every absolute path in a JSON-shaped value."""
    if isinstance(value, str):
        return local_path(value)
    if isinstance(value, list):
        return [local_paths(item) for item in value]
    if isinstance(value, dict):
        return {key: local_paths(item) for key, item in value.items()}
    return value
