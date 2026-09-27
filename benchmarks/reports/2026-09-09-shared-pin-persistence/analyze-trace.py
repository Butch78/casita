#!/usr/bin/env python3
"""Summarize JSON CLOSE spans from the closure example or Obrador plugin.

Span durations include nested work; do not sum them to obtain wall time.
"""

import argparse
import collections
import datetime
import gzip
import json
import re
import sys


def milliseconds(value):
    match = re.fullmatch(r"([0-9.]+)(ns|µs|ms|s)", value)
    if match is None:
        raise ValueError(f"unknown span duration: {value!r}")
    return float(match[1]) * {"ns": 0.000001, "µs": 0.001, "ms": 1, "s": 1000}[match[2]]


def summarize(path, diagnostics=False):
    totals = collections.defaultdict(lambda: {"count": 0, "inclusive_ms": 0})
    phases = collections.defaultdict(list)
    callers = collections.defaultdict(lambda: collections.defaultdict(list))
    first = last = None
    opener = gzip.open if str(path).endswith(".gz") else open
    with opener(path, "rt") as source:
        for line in source:
            event = json.loads(line)
            fields = event.get("fields", event)
            if diagnostics:
                timestamp = event.get("timestamp")
                if timestamp:
                    timestamp = datetime.datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
                    first = min(first, timestamp) if first else timestamp
                    last = max(last, timestamp) if last else timestamp
                if event.get("target") == "casita::pin_timing":
                    phase = fields["phase"]
                    elapsed = float(fields["elapsed_seconds"]) * 1000
                    phases[phase].append(elapsed)
                    caller = next((span["name"] for span in event.get("spans", [])
                                   if span.get("name", "").startswith("plugin.")), "unscoped")
                    callers[caller][phase].append(elapsed)
            name = event.get("span", {}).get("name")
            if name and fields.get("message") == "close":
                totals[name]["count"] += 1
                totals[name]["inclusive_ms"] += sum(
                    milliseconds(fields.get(key, "0ms"))
                    for key in ("time.busy", "time.idle")
                )
    spans = dict(sorted(totals.items()))
    if not diagnostics:
        return spans

    def distribution(values):
        values = sorted(values)
        return {"count": len(values), "total_ms": sum(values),
                "p50_ms": values[(len(values) - 1) // 2],
                "p95_ms": values[(95 * len(values) + 99) // 100 - 1],
                "max_ms": values[-1]}

    return {"spans": spans,
            "trace_window_seconds": (last - first).total_seconds() if first else None,
            "pin_phases": {name: distribution(values) for name, values in sorted(phases.items())},
            "pin_phases_by_plugin_call": {
                caller: {name: distribution(values) for name, values in sorted(items.items())}
                for caller, items in sorted(callers.items())},
            "timing_note": "Spans and pin phases nest and overlap; totals are not additive wall time. "
                           "Unscoped pin events may run on workers without propagated span context."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace")
    parser.add_argument("--diagnostics", action="store_true", help="include Casita pin phases and plugin attribution")
    args = parser.parse_args()
    json.dump(summarize(args.trace, args.diagnostics), sys.stdout, indent=2)
    print()
