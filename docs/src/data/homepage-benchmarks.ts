import baselineJson from '../../../benchmarks/baselines/2026-08-29-45699f8-linux-x86_64.json?raw';

// Use the authoritative published baseline at build time. Raw samples never
// enter the client bundle; the homepage renders only the selected aggregates.
const filename = '2026-08-29-45699f8-linux-x86_64.json';
interface Aggregate {
  corpus: string;
  cache_policy: string;
  operation: string;
  implementation: string;
  median_wall_seconds: number;
  p95_wall_seconds: number;
  samples: number;
}
interface Baseline {
  aggregates: Aggregate[];
  corpora: Record<string, { base_bytes: number }>;
  environment: {
    captured_at_utc: string;
    casita_revision: string;
    cpu: string;
    memory_bytes: number;
    cpu_governor: string;
  };
  configuration: { repetitions: number };
  tools: Record<string, { version: string }>;
}
const baseline: Baseline = JSON.parse(baselineJson);

export const sourceUrl = `https://github.com/cachix/casita/blob/main/benchmarks/baselines/${filename}`;
export const environment = baseline.environment;
export const repetitions = baseline.configuration.repetitions;
export const tools = baseline.tools;
export const workloads = [
  { id: 'mixed', name: 'Mixed files' },
  { id: 'small-files', name: 'Small files' },
  { id: 'large-files', name: 'Large files' },
].map(workload => ({ ...workload, size: (baseline.corpora[workload.id].base_bytes / 2 ** 20).toFixed(1) }));
export const operations = [
  { id: 'cold-import', name: 'First import', description: 'Import into an empty repository.' },
  { id: 'edited-import', name: 'Small update', description: 'Re-import an edited copy of the tree.' },
  { id: 'sync-warm', name: 'Sync the update', description: 'Local sync to a destination with the base tree.' },
];
const implementations = [
  { id: 'casita', name: 'Casita' }, { id: 'git', name: 'Git' },
  { id: 'restic', name: 'restic' }, { id: 'borg', name: 'Borg' },
  { id: 'tar-zstd', name: 'tar + zstd' },
];
export const comparisons = workloads.flatMap(workload => ['warm', 'cold'].map(cache => ({
  workload: workload.id, cache,
  charts: operations.map(operation => {
    const rows = implementations.map(implementation => {
      const result = baseline.aggregates.find(row => row.corpus === workload.id
        && row.cache_policy === cache && row.operation === operation.id
        && row.implementation === implementation.id);
      if (!result && !(operation.id === 'sync-warm' && ['borg', 'tar-zstd'].includes(implementation.id))) {
        throw new Error(`Missing homepage benchmark: ${workload.id}/${cache}/${operation.id}/${implementation.id}`);
      }
      return { ...implementation, result };
    });
    const ceiling = Math.ceil(Math.max(...rows.map(row => row.result?.median_wall_seconds ?? 0)) * 10) / 10;
    return { ...operation, rows, ceiling };
  }),
})));
