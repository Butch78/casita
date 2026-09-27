"""Stable benchmark metric identities and comparison policy."""

from __future__ import annotations

import math
import hashlib
import re
from dataclasses import dataclass
from typing import Any, Mapping, Sequence
from urllib.parse import quote


class MetricRegistryError(RuntimeError):
    pass


@dataclass(frozen=True)
class MetricDefinition:
    id: str
    measure: str
    unit: str
    direction: str
    comparison: str
    gate: bool


def load_metric_registry(manifest: Mapping[str, Any]) -> dict[str, MetricDefinition]:
    rows = manifest.get("metrics")
    if not isinstance(rows, list) or not rows:
        raise MetricRegistryError("benchmark manifest has no metrics array")

    registry: dict[str, MetricDefinition] = {}
    measures: set[str] = set()
    for row in rows:
        if not isinstance(row, dict):
            raise MetricRegistryError(f"invalid metric definition: {row!r}")
        identifier = row.get("id")
        measure = row.get("measure")
        unit = row.get("unit")
        direction = row.get("direction")
        comparison = row.get("comparison")
        gate = row.get("gate")
        if not isinstance(identifier, str) or not identifier or identifier in registry:
            raise MetricRegistryError(f"invalid or duplicate metric id: {identifier!r}")
        if not isinstance(measure, str) or not measure or len(measure) > 64:
            raise MetricRegistryError(f"metric {identifier} has invalid measure {measure!r}")
        if measure in measures:
            raise MetricRegistryError(f"duplicate metric measure: {measure!r}")
        if not isinstance(unit, str) or not unit:
            raise MetricRegistryError(f"metric {identifier} has no unit")
        if direction not in {"lower", "higher", "neutral"}:
            raise MetricRegistryError(f"metric {identifier} has invalid direction {direction!r}")
        if comparison not in {"statistical", "exact", "informational"}:
            raise MetricRegistryError(
                f"metric {identifier} has invalid comparison policy {comparison!r}"
            )
        if not isinstance(gate, bool):
            raise MetricRegistryError(f"metric {identifier} has non-boolean gate")
        if gate and direction == "neutral":
            raise MetricRegistryError(f"gated metric {identifier} needs a direction")
        registry[identifier] = MetricDefinition(
            id=identifier,
            measure=measure,
            unit=unit,
            direction=direction,
            comparison=comparison,
            gate=gate,
        )
        measures.add(measure)
    return registry


IDENTITY_FIELDS = (
    "suite_id",
    "workload",
    "profile",
    "cache_policy",
    "operation",
    "implementation",
)


def observation_identity(run: Mapping[str, Any], observation: Mapping[str, Any]) -> tuple[str, ...]:
    values = {"suite_id": run.get("suite_id"), **observation}
    identity = tuple(str(values.get(field, "unknown")) for field in IDENTITY_FIELDS)
    if any(not value for value in identity):
        raise MetricRegistryError(f"incomplete benchmark identity: {identity!r}")
    return identity


def benchmark_name(identity: Sequence[str]) -> str:
    if len(identity) != len(IDENTITY_FIELDS):
        raise MetricRegistryError(f"invalid benchmark identity: {identity!r}")
    return "/".join(
        f"{field}={quote(str(value), safe='-_.~')}"
        for field, value in zip(IDENTITY_FIELDS, identity, strict=True)
    )


def bencher_benchmark_name(identity: Sequence[str]) -> str:
    """Return a readable, collision-resistant Bencher name below its slug limit."""
    if len(identity) != len(IDENTITY_FIELDS):
        raise MetricRegistryError(f"invalid benchmark identity: {identity!r}")
    suite, _workload, _profile, _cache, operation, implementation = identity

    def slug(value: str, limit: int) -> str:
        rendered = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
        return (rendered or "unknown")[:limit].rstrip("-")

    digest = hashlib.sha256("\0".join(identity).encode()).hexdigest()[:12]
    name = f"{slug(suite, 14)}-{slug(operation, 16)}-{slug(implementation, 12)}-{digest}"
    if len(name) > 64:
        raise AssertionError(f"Bencher benchmark name exceeds 64 characters: {name}")
    return name


def registered_metrics(
    observation: Mapping[str, Any], registry: Mapping[str, MetricDefinition]
) -> dict[str, float]:
    selected: dict[str, float] = {}
    for name, value in observation.get("metrics", {}).items():
        if name not in registry:
            continue
        if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value):
            raise MetricRegistryError(f"invalid registered metric {name}={value!r}")
        selected[name] = float(value)
    return selected


def bmf_document(
    runs: Sequence[Mapping[str, Any]], registry: Mapping[str, MetricDefinition]
) -> dict[str, dict[str, dict[str, float]]]:
    document: dict[str, dict[str, dict[str, float]]] = {}
    for run in runs:
        for observation in run.get("observations", []):
            if observation.get("status") != "ok":
                continue
            identity = observation_identity(run, observation)
            name = bencher_benchmark_name(identity)
            metrics = registered_metrics(observation, registry)
            if not metrics:
                continue
            if name in document:
                raise MetricRegistryError(
                    f"duplicate benchmark identity {name}; export one result per commit/testbed"
                )
            document[name] = {
                registry[metric].measure: {"value": value}
                for metric, value in sorted(metrics.items())
            }
    return document
