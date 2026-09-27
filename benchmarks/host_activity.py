"""Linux process sampling for repeatable runs on a shared benchmark host."""
from __future__ import annotations

import os
import pathlib
import threading
import time

from benchmarks.suites.repository import BenchmarkError


def processes():
    result = {}
    for path in pathlib.Path('/proc').glob('[0-9]*/stat'):
        try:
            raw = path.read_text()
            name, fields = raw[raw.index('(') + 1:raw.rindex(')')], raw[raw.rindex(')') + 2:].split()
            result[int(path.parent.name)] = dict(name=name, state=fields[0], parent=int(fields[1]),
                ticks=int(fields[11]) + int(fields[12]), started=int(fields[19]))
        except (OSError, ValueError, IndexError):
            continue  # Processes may exit between directory enumeration and read.
    return result


def activity(before, after, elapsed, owner):
    own = {owner}
    while True:
        children = {pid for pid, p in after.items() if p['parent'] in own}
        if children <= own:
            break
        own |= children
    competitors, paused, ticks, kernel_ticks, consumers = [], [], 0, 0, []
    for pid, p in after.items():
        if pid in own:
            continue
        name = p['name']
        previous = before.get(pid)
        stopped = (previous is not None and previous['started'] == p['started']
                   and previous.get('state') in ('T', 't') and p.get('state') in ('T', 't')
                   and previous['ticks'] == p['ticks'])
        if (name in {'cargo', '.cargo-wrapped', 'rustc', 'rust-lld', 'cc1', 'cc1plus', 'nix',
                     'clippy-driver', 'cargo-clippy', '.cargo-clippy-w', 'rustdoc', 'ld.lld'}
                or name.startswith(('casita-', 'obrador-reads-', 'online_holds', 'retained_reader',
                                    'tar_import', 'tar-baseline', 'tar-candidate'))):
            (paused if stopped else competitors).append({'pid': pid, 'name': name})
        if previous and previous['started'] == p['started']:
            delta = max(0, p['ticks'] - previous['ticks'])
            if p['parent'] == 2 or pid == 2:
                # Filesystem workers can execute the benchmark's own I/O.
                # Record this CPU separately; ownership cannot be inferred.
                kernel_ticks += delta
            else:
                ticks += delta
                if delta:
                    consumers.append({'pid': pid, 'name': name, 'ticks': delta})
    denominator = os.sysconf('SC_CLK_TCK') * elapsed * (os.cpu_count() or 1)
    return {'external_cpu_fraction': ticks / denominator,
            'kernel_worker_cpu_fraction': kernel_ticks / denominator,
            'top_external_processes': sorted(consumers, key=lambda p: p['ticks'], reverse=True)[:8],
            'competing_processes': competitors, 'paused_build_processes': paused}


class QuietHost:
    """Wait for a quiet interval, then record competition during one case."""
    def __init__(self, timeout=900, quiet_seconds=10, max_cpu_fraction=0.05,
                 allow_competing_builds=False):
        if not pathlib.Path('/proc/self/stat').exists():
            raise BenchmarkError('quiet-host sampling requires Linux /proc')
        self.timeout, self.quiet_seconds, self.max_cpu_fraction = timeout, quiet_seconds, max_cpu_fraction
        self.allow_competing_builds = allow_competing_builds
        self.stop = threading.Event()
        self.samples = []

    def sample(self):
        start = time.monotonic()
        before = processes()
        self.stop.wait(1)
        after = processes()
        elapsed = time.monotonic() - start
        return {**activity(before, after, elapsed, os.getpid()), 'interval_seconds': elapsed}

    def quiet(self, sample):
        return ((self.allow_competing_builds or not sample['competing_processes'])
                and sample['external_cpu_fraction'] <= self.max_cpu_fraction)

    def __enter__(self):
        start, quiet_since = time.monotonic(), None
        while True:
            row = self.sample()
            now = time.monotonic()
            quiet_since = (quiet_since or now) if self.quiet(row) else None
            if quiet_since is not None and now - quiet_since >= self.quiet_seconds:
                break
            if now - start >= self.timeout:
                raise BenchmarkError('host did not become quiet before the deadline')
        self.wait_seconds = time.monotonic() - start
        def monitor():
            while not self.stop.is_set():
                row = self.sample()
                if row['interval_seconds'] >= 0.5:
                    self.samples.append(row)
        self.thread = threading.Thread(target=monitor, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.thread.join()

    def report(self):
        return {'quiet': bool(self.samples) and all(self.quiet(row) for row in self.samples),
                'wait_seconds': self.wait_seconds, 'max_external_cpu_fraction': self.max_cpu_fraction,
                'allow_competing_builds': self.allow_competing_builds,
                'samples': self.samples}
