#!/usr/bin/env python3
"""Compare normalized benchmark artifacts and export Bencher Metric Format."""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import statistics
import sys
from collections.abc import Sequence
from typing import Any, Mapping

from benchmarks import dashboard
from benchmarks import metrics as metric_policy
from benchmarks.suites import repository as common


COMPARISON_SCHEMA = "casita.benchmark-comparison.v1"
SERIES_SCHEMA = "casita.benchmark-revision-series.v1"


class ComparisonError(RuntimeError):
    pass


def normalize_many(paths: Sequence[pathlib.Path]) -> list[dict[str, Any]]:
    if not paths:
        raise ComparisonError("at least one result is required")
    return [dashboard.normalize_result(path) for path in paths]


def revision_of(runs: Sequence[Mapping[str, Any]]) -> str | None:
    revisions = {
        str(run.get("environment", {}).get("casita_revision"))
        for run in runs
        if run.get("environment", {}).get("casita_revision")
    }
    if len(revisions) > 1:
        raise ComparisonError(f"result set contains multiple revisions: {sorted(revisions)}")
    return next(iter(revisions), None)


def indexed_observations(
    runs: Sequence[Mapping[str, Any]],
) -> dict[tuple[str, ...], Mapping[str, Any]]:
    indexed: dict[tuple[str, ...], Mapping[str, Any]] = {}
    for run in runs:
        for observation in run.get("observations", []):
            identity = metric_policy.observation_identity(run, observation)
            if identity in indexed:
                raise ComparisonError(
                    "duplicate observation identity: " + metric_policy.benchmark_name(identity)
                )
            indexed[identity] = observation
    return indexed


def coalesce_repeated_runs(
    runs: Sequence[Mapping[str, Any]],
    registry: Mapping[str, metric_policy.MetricDefinition],
) -> dict[str, Any]:
    """Reduce interleaved one-repetition runs into one normalized revision run."""
    if not runs:
        raise ComparisonError("at least one normalized run is required")
    revision = revision_of(runs)
    suite_ids = {str(run.get("suite_id")) for run in runs}
    if len(suite_ids) != 1:
        raise ComparisonError(f"result set contains multiple suites: {sorted(suite_ids)}")

    grouped: dict[tuple[str, ...], list[Mapping[str, Any]]] = {}
    for run in runs:
        for observation in run.get("observations", []):
            identity = metric_policy.observation_identity(run, observation)
            grouped.setdefault(identity, []).append(observation)

    observations = []
    for identity, repeated in sorted(grouped.items()):
        successful = [row for row in repeated if row.get("status") == "ok"]
        repeated_metrics = [metric_policy.registered_metrics(row, registry) for row in successful]
        metrics: dict[str, float] = {}
        for metric in registry:
            values = [row[metric] for row in repeated_metrics if metric in row]
            if len(values) != len(repeated):
                continue
            metrics[metric] = (
                sorted(values)[max(0, math.ceil(0.95 * len(values)) - 1)]
                if metric.startswith("p95_")
                else statistics.median(values)
            )
        observations.append(
            {
                **dict(zip(metric_policy.IDENTITY_FIELDS[1:], identity[1:], strict=True)),
                "status": "ok" if len(successful) == len(repeated) else "failed",
                "samples": sum(int(row.get("samples", 1)) for row in repeated),
                "successful_rounds": len(successful),
                "rounds": len(repeated),
                "metrics": metrics,
            }
        )

    return {
        "result_schema": "casita.normalized-repeated-run.v1",
        "suite_id": next(iter(suite_ids)),
        "environment": {
            **dict(runs[0].get("environment", {})),
            "casita_revision": revision,
        },
        "observations": observations,
    }


def classify_change(base: float, head: float, direction: str) -> tuple[float | None, str]:
    if base == 0:
        if head == 0:
            return None, "unchanged"
        if direction == "neutral":
            return None, "informational"
        return None, "increased" if head > base else "decreased"
    change_percent = (head - base) / abs(base) * 100.0
    if head == base:
        outcome = "unchanged"
    elif direction == "neutral":
        outcome = "informational"
    else:
        outcome = "increased" if head > base else "decreased"
    return change_percent, outcome


def compare_runs(
    base_runs: Sequence[Mapping[str, Any]],
    head_runs: Sequence[Mapping[str, Any]],
    registry: Mapping[str, metric_policy.MetricDefinition],
) -> dict[str, Any]:
    base = indexed_observations(base_runs)
    head = indexed_observations(head_runs)
    shared = sorted(base.keys() & head.keys())
    comparisons = []
    skipped_failed = 0
    for identity in shared:
        base_observation = base[identity]
        head_observation = head[identity]
        if base_observation.get("status") != "ok" or head_observation.get("status") != "ok":
            skipped_failed += 1
            continue
        base_metrics = metric_policy.registered_metrics(base_observation, registry)
        head_metrics = metric_policy.registered_metrics(head_observation, registry)
        for metric in sorted(base_metrics.keys() & head_metrics.keys()):
            definition = registry[metric]
            base_value = base_metrics[metric]
            head_value = head_metrics[metric]
            change_percent, outcome = classify_change(
                base_value, head_value, definition.direction
            )
            comparisons.append(
                {
                    "benchmark": metric_policy.benchmark_name(identity),
                    "identity": dict(zip(metric_policy.IDENTITY_FIELDS, identity, strict=True)),
                    "metric": metric,
                    "measure": definition.measure,
                    "unit": definition.unit,
                    "direction": definition.direction,
                    "comparison": definition.comparison,
                    "gate": definition.gate,
                    "base_value": base_value,
                    "head_value": head_value,
                    "change_percent": change_percent,
                    "outcome": outcome,
                }
            )

    return {
        "result_schema": COMPARISON_SCHEMA,
        "base_revision": revision_of(base_runs),
        "head_revision": revision_of(head_runs),
        "matched_observations": len(shared),
        "base_only_observations": len(base.keys() - head.keys()),
        "head_only_observations": len(head.keys() - base.keys()),
        "skipped_failed_observations": skipped_failed,
        "comparisons": comparisons,
        "summary": {
            outcome: sum(row["outcome"] == outcome for row in comparisons)
            for outcome in ("increased", "decreased", "unchanged", "informational")
        },
    }


def revision_series(
    revision_runs: Sequence[tuple[str, Sequence[Mapping[str, Any]]]],
    registry: Mapping[str, metric_policy.MetricDefinition],
    baseline_label: str | None = None,
) -> dict[str, Any]:
    if len(revision_runs) < 2:
        raise ComparisonError("revision series requires at least two revisions")
    labels = [label for label, _runs in revision_runs]
    if len(labels) != len(set(labels)):
        raise ComparisonError("revision labels must be unique")
    baseline_label = baseline_label or labels[0]
    if baseline_label not in labels:
        raise ComparisonError(f"unknown baseline label {baseline_label!r}")

    coalesced = [
        (label, coalesce_repeated_runs(runs, registry))
        for label, runs in revision_runs
    ]
    indexed = [indexed_observations([run]) for _label, run in coalesced]
    revisions = [revision_of([run]) for _label, run in coalesced]
    baseline_index = labels.index(baseline_label)
    identities = sorted(set().union(*(set(rows) for rows in indexed)))
    rows = []
    for identity in identities:
        metric_names: set[str] = set()
        for observations in indexed:
            observation = observations.get(identity)
            if observation is not None:
                metric_names.update(metric_policy.registered_metrics(observation, registry))
        for metric in sorted(metric_names):
            definition = registry[metric]
            values = []
            baseline_observation = indexed[baseline_index].get(identity)
            baseline_metrics = (
                metric_policy.registered_metrics(baseline_observation, registry)
                if baseline_observation and baseline_observation.get("status") == "ok"
                else {}
            )
            baseline_value = baseline_metrics.get(metric)
            for index, (label, _run) in enumerate(coalesced):
                observation = indexed[index].get(identity)
                status = "missing" if observation is None else str(observation.get("status"))
                observed = (
                    metric_policy.registered_metrics(observation, registry).get(metric)
                    if observation is not None and status == "ok"
                    else None
                )
                previous_value = None
                if index:
                    previous = indexed[index - 1].get(identity)
                    if previous is not None and previous.get("status") == "ok":
                        previous_value = metric_policy.registered_metrics(previous, registry).get(metric)

                baseline_change = baseline_outcome = None
                if observed is not None and baseline_value is not None:
                    baseline_change, baseline_outcome = classify_change(
                        baseline_value, observed, definition.direction
                    )
                previous_change = previous_outcome = None
                if observed is not None and previous_value is not None:
                    previous_change, previous_outcome = classify_change(
                        previous_value, observed, definition.direction
                    )
                values.append(
                    {
                        "label": label,
                        "revision": revisions[index],
                        "status": status,
                        "value": observed,
                        "change_from_baseline_percent": baseline_change,
                        "outcome_from_baseline": baseline_outcome,
                        "change_from_previous_percent": previous_change,
                        "outcome_from_previous": previous_outcome,
                    }
                )
            rows.append(
                {
                    "benchmark": metric_policy.benchmark_name(identity),
                    "identity": dict(zip(metric_policy.IDENTITY_FIELDS, identity, strict=True)),
                    "metric": metric,
                    "measure": definition.measure,
                    "unit": definition.unit,
                    "direction": definition.direction,
                    "comparison": definition.comparison,
                    "gate": definition.gate,
                    "values": values,
                }
            )

    return {
        "result_schema": SERIES_SCHEMA,
        "baseline_label": baseline_label,
        "revisions": [
            {
                "label": label,
                "revision": revision,
                "rounds": len(runs),
                "normalized": run,
            }
            for (label, runs), revision, (_coalesced_label, run) in zip(
                revision_runs, revisions, coalesced, strict=True
            )
        ],
        "rows": rows,
    }


def render_series_report(result: Mapping[str, Any]) -> str:
    revisions = result["revisions"]
    labels = [str(revision["label"]) for revision in revisions]
    lines = [
        "# Multi-revision benchmark comparison",
        "",
        f"Baseline: `{result['baseline_label']}`. Values in parentheses are changes from that baseline.",
        "Outcomes are directional only; threshold-backed regressions are Bencher alerts.",
        "",
        "| Benchmark | Metric | " + " | ".join(labels) + " |",
        "|---|---|" + "---:|" * len(labels),
    ]
    for row in result["rows"]:
        cells = []
        for value in row["values"]:
            observed = value["value"]
            if observed is None:
                cells.append(str(value["status"]))
                continue
            change = value["change_from_baseline_percent"]
            suffix = "" if change is None else f" ({change:+.2f}%)"
            cells.append(f"{observed:.6g}{suffix}")
        lines.append(
            f"| `{row['benchmark']}` | {row['metric']} ({row['unit']}) | "
            + " | ".join(cells)
            + " |"
        )
    if len(labels) > 1:
        transitions = [
            f"{previous} → {current}" for previous, current in zip(labels, labels[1:])
        ]
        lines.extend(
            [
                "",
                "## Changes from the preceding revision",
                "",
                "| Benchmark | Metric | " + " | ".join(transitions) + " |",
                "|---|---|" + "---:|" * len(transitions),
            ]
        )
        for row in result["rows"]:
            cells = []
            for value in row["values"][1:]:
                change = value["change_from_previous_percent"]
                outcome = value["outcome_from_previous"]
                cells.append("n/a" if change is None else f"{change:+.2f}% ({outcome})")
            lines.append(
                f"| `{row['benchmark']}` | {row['metric']} ({row['unit']}) | "
                + " | ".join(cells)
                + " |"
            )
    return "\n".join(lines) + "\n"


def render_report(result: Mapping[str, Any]) -> str:
    lines = [
        "# Paired benchmark comparison",
        "",
        f"- Base revision: `{result.get('base_revision') or 'unknown'}`",
        f"- Head revision: `{result.get('head_revision') or 'unknown'}`",
        f"- Matched observations: {result['matched_observations']}",
        f"- Base-only observations: {result['base_only_observations']}",
        f"- Head-only observations: {result['head_only_observations']}",
        f"- Failed matched observations skipped: {result['skipped_failed_observations']}",
        "- Outcomes are directional only; threshold-backed regressions are Bencher alerts.",
        "",
        "| Benchmark | Metric | Base | Head | Change | Outcome | Policy |",
        "|---|---|---:|---:|---:|---|---|",
    ]
    for row in result["comparisons"]:
        change = "n/a" if row["change_percent"] is None else f"{row['change_percent']:+.2f}%"
        lines.append(
            f"| `{row['benchmark']}` | {row['metric']} ({row['unit']}) | "
            f"{row['base_value']:.6g} | {row['head_value']:.6g} | {change} | "
            f"{row['outcome']} | {row['comparison']} |"
        )
    return "\n".join(lines) + "\n"


def write_json(path: pathlib.Path | None, value: Mapping[str, Any]) -> None:
    rendered = json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if path is None:
        sys.stdout.write(rendered)
    else:
        common.write_atomic(path, rendered)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=pathlib.Path("benchmarks/manifest.json"))
    subparsers = parser.add_subparsers(dest="command", required=True)

    export = subparsers.add_parser("export-bencher", help="export normalized results as BMF JSON")
    export.add_argument("--result", action="append", type=pathlib.Path, required=True)
    export.add_argument("--output", type=pathlib.Path)

    compare = subparsers.add_parser("compare", help="compare base and head result sets")
    compare.add_argument("--base-result", action="append", type=pathlib.Path, required=True)
    compare.add_argument("--head-result", action="append", type=pathlib.Path, required=True)
    compare.add_argument("--output", type=pathlib.Path)
    compare.add_argument("--report", type=pathlib.Path)
    compare.add_argument("--bencher-output", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        manifest = dashboard.load_manifest(args.manifest)
        registry = metric_policy.load_metric_registry(manifest)
        if args.command == "export-bencher":
            write_json(args.output, metric_policy.bmf_document(normalize_many(args.result), registry))
            return 0

        base_runs = normalize_many(args.base_result)
        head_runs = normalize_many(args.head_result)
        result = compare_runs(base_runs, head_runs, registry)
        write_json(args.output, result)
        if args.report:
            common.write_atomic(args.report, render_report(result))
        if args.bencher_output:
            write_json(args.bencher_output, metric_policy.bmf_document(head_runs, registry))
        return 0
    except (
        ComparisonError,
        dashboard.DashboardError,
        metric_policy.MetricRegistryError,
        OSError,
        json.JSONDecodeError,
    ) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
