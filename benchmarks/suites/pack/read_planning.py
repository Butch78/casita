"""Deterministic I/O model of bounded read planning; not a latency benchmark.

Replay audited physical chunk traces with a plain compressed-chunk LRU.
The eager batch completes all requests before yielding its first chunk. Request
waves assume four concurrent GETs and a barrier between windows. No networking,
decompression, pinning, cancellation, or actual allocator is simulated.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import pathlib

from benchmarks import cli
from benchmarks.suites import repository as common

MIB = 2**20
DEFAULT_TRACE = cli.ROOT / "benchmarks/fixtures/pack-read-planning.json"


def validate_trace(item):
    chunks = item["read_plan"]
    if not chunks or len(chunks) != item["chunks"]:
        raise ValueError("incomplete physical read plan")
    packs, identities = {}, {}
    for c in chunks:
        for field in ("size", "pack_len", "framed_len"):
            if type(c[field]) is not int or c[field] <= 0:
                raise ValueError("invalid chunk extent")
        if type(c["offset"]) is not int or c["offset"] < 0 or c["offset"] + c["framed_len"] > c["pack_len"]:
            raise ValueError("chunk outside pack")
        if not isinstance(c["digest"], str) or not c["digest"] or not isinstance(c["pack"], str) or not c["pack"]:
            raise ValueError("missing chunk or pack identity")
        if packs.setdefault(c["pack"], c["pack_len"]) != c["pack_len"]:
            raise ValueError("inconsistent pack size")
        if identities.setdefault(c["digest"], c) != c:
            raise ValueError("ambiguous physical chunk identity")
    if sum(c["size"] for c in chunks) != item["file_bytes"]:
        raise ValueError("logical size mismatch")
    if len(packs) != item["referenced_packs"] or sum(packs.values()) != item["referenced_pack_bytes"] or max(packs.values()) != item["largest_pack_bytes"]:
        raise ValueError("pack inventory mismatch")
    for pack in packs:
        extents = sorted((c["offset"], c["offset"] + c["framed_len"]) for c in identities.values() if c["pack"] == pack)
        if any(a[1] > b[0] for a, b in zip(extents, extents[1:])):
            raise ValueError("overlapping distinct chunks")
    return chunks


def windows(chunks, budget, field="size"):
    """Bound chosen bytes; an indivisible oversized chunk occupies one window."""
    current, size = [], 0
    for c in chunks:
        if current and size + c[field] > budget:
            yield current
            current, size = [], 0
        current.append(c)
        size += c[field]
    if current:
        yield current


def requests(chunks, max_gap, max_span):
    """Merge neighbors, or use one covering span per pack when max_gap=None."""
    unique = {c["digest"]: c for c in chunks}
    result = []
    if max_gap is None:
        by_pack = collections.defaultdict(list)
        for c in sorted(unique.values(), key=lambda c: (c["pack"], c["offset"])):
            by_pack[c["pack"]].append(c)
        for pack, group in by_pack.items():
            start, end = group[0]["offset"], group[-1]["offset"] + group[-1]["framed_len"]
            useful = sum(c["framed_len"] for c in group)
            if end-start <= max_span and (end-start-useful)*4 <= useful:
                result.append(dict(pack=pack, start=start, end=end, useful=useful, chunks=group))
            else:
                result.extend(requests(group, 0, max_span))
    ordered = [] if max_gap is None else sorted(unique.values(), key=lambda c: (c["pack"], c["offset"]))
    for c in ordered:
        end = c["offset"] + c["framed_len"]
        previous = result[-1] if result else None
        if previous and previous["pack"] == c["pack"]:
            gap = c["offset"] - previous["end"]
            useful = previous["useful"] + c["framed_len"]
            span = end - previous["start"]
            if 0 <= gap <= max_gap and span <= max_span and (span - useful) * 4 <= useful:
                previous.update(end=end, useful=useful)
                previous["chunks"].append(c)
                continue
        result.append(dict(pack=c["pack"], start=c["offset"], end=end,
                           useful=c["framed_len"], chunks=[c]))
    # Independent coverage/accounting gates; no digest verification is claimed.
    covered = []
    for r in result:
        for c in r["chunks"]:
            assert r["pack"] == c["pack"] and 0 <= r["start"] <= c["offset"]
            assert c["offset"] + c["framed_len"] <= r["end"] <= c["pack_len"]
            covered.append(c["digest"])
        assert r["useful"] == sum(c["framed_len"] for c in r["chunks"])
        assert (r["end"] - r["start"] - r["useful"]) * 4 <= r["useful"]
    assert len(covered) == len(set(covered)) and set(covered) == set(unique)
    return result


class Cache:
    def __init__(self, capacity):
        self.capacity, self.used, self.peak = capacity, 0, 0
        self.entries = collections.OrderedDict()

    def get(self, c):
        if c["digest"] not in self.entries:
            return False
        self.entries.move_to_end(c["digest"])
        return True

    def insert(self, c):
        size = c["framed_len"]
        if self.get(c) or size > self.capacity:
            return
        while self.used + size > self.capacity:
            _, removed = self.entries.popitem(last=False)
            self.used -= removed
        self.entries[c["digest"]] = size
        self.used += size
        self.peak = max(self.peak, self.used)
        assert self.used == sum(self.entries.values()) <= self.capacity


def replay(chunks, window_bytes, gap_bytes, cache, pattern, basis="decoded"):
    field = {"decoded": "size", "compressed": "framed_len"}[basis]
    batches = list(windows(chunks, window_bytes, field))
    if pattern == "sequential":
        schedule = [(batch, batch) for batch in batches]
    elif pattern == "interleaved-scans":
        # Deterministically alternate two readers; this is not simultaneous I/O.
        schedule = [(batch, batch) for pair in zip(batches, reversed(batches)) for batch in pair]
    elif pattern == "seek-one":
        # Each unrelated seek consumes one chunk after eagerly fetching a batch.
        # Do NOT combine knowledge of future seeks into one optimistic plan.
        schedule = [(next(windows(chunks[i:], window_bytes, field)), [chunks[i]])
                    for i in range(0, len(chunks), 16)]
    else:
        raise ValueError(pattern)
    metrics = dict(requests=0, fetched_bytes=0, gap_bytes=0, consumed_bytes=0, consumed_framed_bytes=0,
                   request_waves=0, max_decoded_window=0, max_compressed_window=0,
                   max_request_bytes=0, windows=len(schedule), first_window_bytes=0)
    for index, (batch, consumed) in enumerate(schedule):
        missing = [c for c in batch if not cache.get(c)]
        planned = requests(missing, gap_bytes, window_bytes)
        fetched = sum(r["end"] - r["start"] for r in planned)
        metrics["requests"] += len(planned)
        metrics["fetched_bytes"] += fetched
        metrics["gap_bytes"] += sum(r["end"] - r["start"] - r["useful"] for r in planned)
        metrics["request_waves"] += (len(planned) + 3) // 4
        metrics["consumed_bytes"] += sum(c["size"] for c in consumed)
        metrics["consumed_framed_bytes"] += sum(c["framed_len"] for c in consumed)
        metrics["max_decoded_window"] = max(metrics["max_decoded_window"], sum(c["size"] for c in batch))
        metrics["max_compressed_window"] = max(metrics["max_compressed_window"], sum(c["framed_len"] for c in batch))
        metrics["max_request_bytes"] = max(metrics["max_request_bytes"], *(r["end"] - r["start"] for r in planned), 0)
        if index == 0:
            metrics["first_window_bytes"] = fetched
        available = {c["digest"] for c in batch if c not in missing}
        available.update(c["digest"] for r in planned for c in r["chunks"])
        assert all(c["digest"] in available for c in consumed)
        # These are separately owned compressed chunks, not slices retaining a
        # larger allocation. Actual implementation would need to copy or charge
        # backing allocations; this model does not measure that cost.
        for c in batch:
            cache.insert(c)
    metrics["cache_peak_bytes"] = cache.peak
    assert metrics[f"max_{basis}_window"] <= max(window_bytes, max(c[field] for c in chunks))
    assert metrics["max_request_bytes"] <= max(window_bytes, max(c["framed_len"] for c in chunks))
    return metrics


def boundary_cases():
    """Permanent cases on both sides of window, gap, and overfetch thresholds."""
    def chunk(label, start, size):
        return dict(digest=label, pack="p", offset=start, framed_len=size, size=size, pack_len=10000)
    samples = []
    for size in (1023, 1024, 1025):
        chunks = [chunk("a", 0, 512), chunk("b", 512, size - 512)]
        count = len(list(windows(chunks, 1024)))
        assert count == (1 if size <= 1024 else 2)
        samples.append(dict(boundary="window", value=size, windows=count))
    for gap in (127, 128, 129):
        count = len(requests([chunk("a", 0, 512), chunk("b", 512 + gap, 512)], 128, 2048))
        assert count == (1 if gap <= 128 else 2)
        samples.append(dict(boundary="gap", value=gap, requests=count))
    for gap in (255, 256, 257):
        count = len(requests([chunk("a", 0, 512), chunk("b", 512 + gap, 512)], 512, 2048))
        assert count == (1 if gap <= 256 else 2)
        samples.append(dict(boundary="overfetch", value=gap, requests=count))
        coverage = len(requests([chunk("a", 0, 512), chunk("b", 512 + gap, 512)], None, 2048))
        assert coverage == (1 if gap <= 256 else 2)
        samples.append(dict(boundary="coverage", value=gap, requests=coverage))
    return samples


def import_source(path):
    """Freeze traces only after revalidating an actual S3 probe's audit gates."""
    from benchmarks.suites.pack.fragmentation_network import parse_prepared, parse_samples
    raw = path.read_bytes()
    source = json.loads(raw)
    if not source["complete"] or len(source["samples"]) != source["expected_samples"]:
        raise ValueError("incomplete trace source")
    prepared = {}
    for process in source["processes"]:
        if process["exit_code"]:
            raise ValueError("failed trace source")
        config = process["config"]
        if config["mode"] == "prepare":
            prepared[config["prefix"]] = parse_prepared(process["stdout"], config)
        else:
            parse_samples(process["stdout"], config)
    if prepared != source["fixtures"]:
        raise ValueError("fixture differs from audited process output")
    traces = {}
    for corpus, fixture in prepared.items():
        for layout in ("history", "fresh"):
            item = fixture[layout]
            validate_trace(item)
            traces[f"{corpus}/{layout}"] = item
    return dict(schema_version=1, source_sha256=hashlib.sha256(raw).hexdigest(),
                source_artifacts=source["artifacts"], corpus_artifact=source.get("corpus_artifact"),
                source_correctness="all source process gates revalidated; full retained history independently hashed",
                traces=traces)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--trace", type=pathlib.Path, default=DEFAULT_TRACE)
    parser.add_argument("--import-source", type=pathlib.Path,
                        help="revalidate a complete s3-fragmentation result and freeze its traces to --trace")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--output", type=pathlib.Path, default=cli.ROOT / "benchmarks/results/pack-read-planning.json")
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("positive repetitions required")
    if args.import_source:
        common.write_atomic(args.trace, json.dumps(import_source(args.import_source), indent=2) + "\n")
    raw = args.trace.read_bytes()
    fixture = json.loads(raw)
    if fixture.get("schema_version") != 1 or not fixture.get("traces"):
        raise ValueError("missing trace corpus")
    result = dict(schema_version=1, result_schema="casita.pack-read-planning.v1", suite_id="blob-backends",
                  complete=False, model_only=True, configuration=dict(profile=args.profile, repetitions=args.repetitions,
                  request_concurrency=4, max_overfetch_fraction=0.25, window_barrier=True),
                  artifacts=[dict(path=str(args.trace), sha256=hashlib.sha256(raw).hexdigest()),
                             dict(path=__file__, sha256=hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest())],
                  boundaries=boundary_cases(), samples=[])
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    sizes = [1, 4] if args.profile == "smoke" else [1, 4, 16, 64]
    for name, item in fixture["traces"].items():
        chunks = validate_trace(item)
        live = sum(c["framed_len"] for c in {c["digest"]: c for c in chunks}.values())
        corpus = name.rsplit("/", 1)[0]
        largest = max(t["largest_pack_bytes"] for key, t in fixture["traces"].items()
                      if key.rsplit("/", 1)[0] == corpus)
        caches = {"disabled": 0, "below-largest-pack": largest - 1,
                  "above-largest-pack": largest + 1, "default": 64*MIB,
                  "below-live-chunks": live-1, "fits-live-chunks": live, "above-live-chunks": live+1}
        for basis in ("decoded", "compressed"):
            for rep in range(1, args.repetitions+1):
                for window in sizes:
                    for gap in (0, 128*1024, None):
                        for cache_name, capacity in caches.items():
                            for pattern in ("sequential", "seek-one", "interleaved-scans"):
                                cache = Cache(capacity)
                                for phase in ("cold", "warm"):
                                    metrics = replay(chunks, window*MIB, gap, cache, pattern, basis)
                                    if phase == "warm" and capacity >= live:
                                        assert metrics["requests"] == 0
                                    result["samples"].append(dict(trace=name, window_basis=basis, window_bytes=window*MIB,
                                        max_gap_bytes=gap, policy={0: "contiguous", 128*1024: "bounded-gap", None: "coverage"}[gap],
                                        cache=cache_name, cache_bytes=capacity, pattern=pattern,
                                        phase=phase, repetition=rep, **metrics))
    result["expected_samples"] = len(fixture["traces"])*2*len(sizes)*3*len(caches)*3*2*args.repetitions
    assert len(result["samples"]) == result["expected_samples"]
    result["complete"] = True
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    print(f"Validated {len(result['samples'])} modeled cases and {len(result['boundaries'])} boundaries; no wall-clock latency claim.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
