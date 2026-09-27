"""Local durable ledger staging, publication protection, deletion claims, and contention."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess
import tempfile
from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.object_reads import choices_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'metadata::pins::persistent::benchmarks::benchmark_durable_ledger'
CORRECTNESS = 'exact retained inventory, staged and publication resources protected, stale and protected claims rejected, no leaked pins or claims'
PHASES = ('register','payload-protect','publication-protect','release','deletion-claim','deletion-finish')
INCREMENTAL_METRICS = ('cached_edits','inventory_copies','inventory_diffs')
METRICS = ('journal_frames','journal_bytes','journal_syncs','checkpoints','journal_adoptions','groups','operations','max_group','replacement_updates')


def parse_sample(stdout, records, writers, iterations, mode, context='quiet', incremental=False):
    try:
        cases = [json.loads(line.removeprefix('durable_ledger_sample ')) for line in stdout.splitlines() if line.startswith('durable_ledger_sample ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid durable ledger JSON') from error
    if 'test result: ok. 1 passed; 0 failed;' not in stdout or len(cases) != 1:
        raise common.BenchmarkError('durable ledger probe must execute exactly one passing test')
    case = cases[0]
    if (not isinstance(case,dict) or case.get('records') != records or case.get('writers') != writers
            or case.get('context','quiet') != context or case.get('iterations') != iterations or case.get('mode') != mode or case.get('correctness') != CORRECTNESS):
        raise common.BenchmarkError('wrong durable ledger configuration or missing correctness gate')
    samples = case.get('samples')
    if not isinstance(samples,list) or len(samples) != iterations * len(PHASES):
        raise common.BenchmarkError('missing ledger phase samples')
    for index,sample in enumerate(samples):
        phase = PHASES[index % len(PHASES)]
        operations = 1 if phase.startswith('deletion-') else writers
        if (not isinstance(sample,dict) or sample.get('phase') != phase or sample.get('iteration') != index // len(PHASES)
                or sample.get('operations') != operations or type(sample.get('nanos')) is not int or sample['nanos'] < 0):
            raise common.BenchmarkError('invalid ledger phase timing')
        metrics = sample.get('metrics',{})
        if not isinstance(metrics,dict) or any(type(metrics.get(key)) is not int or metrics[key] < 0 for key in METRICS):
            raise common.BenchmarkError('missing ledger durability metrics')
        if metrics['operations'] != operations or not 1 <= metrics['max_group'] <= 64 or not 1 <= metrics['groups'] <= operations:
            raise common.BenchmarkError('invalid ledger group bound')
        if incremental:
            if any(type(metrics.get(key)) is not int or metrics[key] < 0 for key in INCREMENTAL_METRICS):
                raise common.BenchmarkError('missing incremental work counters')
            if metrics['cached_edits'] != operations or metrics['inventory_copies'] or metrics['inventory_diffs']:
                raise common.BenchmarkError('warm journal operation copied or compared the whole inventory')
        if mode == 'replacement' and metrics['replacement_updates'] != operations:
            raise common.BenchmarkError('replacement control skipped durable updates')
        if mode == 'journal' and (metrics['replacement_updates'] or metrics['journal_syncs'] < 1):
            raise common.BenchmarkError('journal skipped its durability boundary')
    return case


def save(args,result):
    common.write_atomic(args.output,json.dumps(result,indent=2)+'\n')
    lines=['# Durable local ledger','',f"Complete: {result['complete']}",'',
           'Each phase measures all concurrent writers completing. Wall seconds is mean phase time per logical operation, not individual request latency.',
           'Publication protection measures catalog pin updates; it excludes payload sealing and metadata publication itself.',
           'The replacement control rewrites the inventory using preallocated slots in the same binary; it is not the historical macOS temporary-file backend. Journal groups preserve queue ordering and wait for sync.',
           'Fixture creation, reopened inventory checks, and rejection gates are outside timings. Process RSS includes them. OS caches are not flushed.',
           'Journal syncs count append/checkpoint barriers; adoption and capacity-growth syncs are separate from this metric.', '',
           '| Records | Writers | Mode | Context | Phase | Repetition | Mean ms/operation |','|---:|---:|---|---|---|---:|---:|']
    if 'error' in result: lines += [result['error'],'']
    for s in result['samples']:
        lines.append(f"| {s['entries']} | {s['writers']} | {s['variant']} | {s['ledger_context']} | {s['operation']} | {s['repetition']} | {s['wall_seconds']*1000:.3f} |")
    common.write_atomic(args.report or args.output.with_suffix('.md'),'\n'.join(lines)+'\n')


def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile',choices=('smoke','standard'),default='standard')
    parser.add_argument('--counts',type=positive_csv)
    parser.add_argument('--writers',type=positive_csv)
    parser.add_argument('--iterations',type=int)
    parser.add_argument('--modes',type=choices_csv(('journal','replacement')),default=['journal','replacement'])
    parser.add_argument('--repetitions',type=int,default=3)
    parser.add_argument('--probe-binary',type=pathlib.Path)
    parser.add_argument('--reference-binary',type=pathlib.Path,help='previous journal probe; compare quiet context using its journal mode')
    parser.add_argument('--contexts',type=choices_csv(('quiet','readers','claims')),default=['quiet'])
    parser.add_argument('--no-build',action='store_true')
    parser.add_argument('--output',type=pathlib.Path,required=True)
    parser.add_argument('--report',type=pathlib.Path)
    args=parser.parse_args(argv)
    counts=args.counts or ([1,64] if args.profile=='smoke' else [1,64,4096,16384])
    writers=args.writers or ([1,8] if args.profile=='smoke' else [1,8,64])
    iterations=args.iterations if args.iterations is not None else (2 if args.profile=='smoke' else 8)
    if min(iterations,args.repetitions)<1: parser.error('iterations and repetitions must be positive')
    if args.no_build and args.probe_binary is None: parser.error('--no-build requires --probe-binary')
    binary=args.probe_binary
    if binary is None:
        built=subprocess.run(['cargo',*CARGO_ARGUMENTS],cwd=cli.ROOT,capture_output=True,text=True)
        if built.returncode: raise common.BenchmarkError(built.stderr or built.stdout)
        binary=parse_probe_binary(built.stdout)
    binary=binary.resolve()
    with binary.open('rb') as handle: digest=hashlib.file_digest(handle,'sha256').hexdigest()
    artifacts=[dict(path=str(binary),sha256=digest)]
    reference=args.reference_binary.resolve() if args.reference_binary else None
    if reference:
        with reference.open('rb') as handle: reference_digest=hashlib.file_digest(handle,'sha256').hexdigest()
        artifacts.append(dict(path=str(reference),sha256=reference_digest))
    with tempfile.TemporaryDirectory(prefix='casita-durable-ledger-') as temporary:
        work=pathlib.Path(temporary)
        result=dict(schema_version=1,result_schema='casita.durable-ledger.v1',suite_id='state-and-publication',complete=False,
                    environment=common.environment_metadata(work), configuration=dict(profile=args.profile,counts=counts,writers=writers,
                    iterations=iterations,modes=args.modes,contexts=args.contexts,reference_binary=str(reference) if reference else None,repetitions=args.repetitions),artifacts=artifacts,samples=[],processes=[])
        save(args,result)
        schedule=[(rep,count,writer,mode,context) for rep in range(1,args.repetitions+1) for count in counts for writer in writers for mode in args.modes for context in args.contexts]
        if reference:
            schedule += [(rep,count,writer,'journal-reference','quiet') for rep in range(1,args.repetitions+1) for count in counts for writer in writers]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition,count,writer,mode,context in schedule:
                probe=reference if mode=='journal-reference' else binary
                probe_mode='journal' if mode=='journal-reference' else mode
                print(f'durable-ledger: {count} records, {writer} writers, {mode}, {context}, repetition {repetition}',flush=True)
                env={**os.environ,'CASITA_LEDGER_RECORDS':str(count),'CASITA_LEDGER_WRITERS':str(writer),
                     'CASITA_LEDGER_ITERATIONS':str(iterations),'CASITA_LEDGER_MODE':probe_mode,'CASITA_LEDGER_CONTEXT':context}
                stdout,stderr=work/'stdout',work/'stderr'
                timing=common.measured_command(common.CommandSpec([[str(probe),PROBE,'--exact','--ignored','--nocapture']],work,env),stdout,stderr,check=False)
                captured=stdout.read_text()
                result['processes'].append({**timing,'records':count,'writers':writer,'mode':mode,'context':context,'repetition':repetition,'stdout':captured,'stderr':stderr.read_text()})
                if timing['exit_code']: raise common.BenchmarkError(f'ledger probe failed: {captured}\n{stderr.read_text()}')
                case=parse_sample(captured,count,writer,iterations,probe_mode,context,incremental=(mode=='journal'))
                for phase in PHASES:
                    samples=[s for s in case['samples'] if s['phase']==phase]
                    operations=sum(s['operations'] for s in samples)
                    metrics={key:sum(s['metrics'][key] for s in samples) for key in (*METRICS, *(INCREMENTAL_METRICS if mode!='journal-reference' else ())) }
                    metrics['max_group']=max(s['metrics']['max_group'] for s in samples)
                    result['samples'].append(dict(status='ok',operation=phase,entries=count,writers=writer,variant=mode,ledger_context=context,repetition=repetition,
                        wall_seconds=sum(s['nanos'] for s in samples)/operations/1e9,max_rss_bytes=timing['max_rss_bytes'],metrics=metrics,correctness=CORRECTNESS))
                save(args,result)
        except Exception as error:
            result['error']=str(error); save(args,result); raise
        result['complete']=True; save(args,result)
    return 0


if __name__=='__main__': raise SystemExit(main())
