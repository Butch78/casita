"""Measured launch-range replay and decoded-cache capacity boundaries."""
import argparse
import hashlib
import json
import os
import platform
import datetime
from pathlib import Path
import random
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'blob::pack::fetch::seek_replay::benchmark_seek_replay'
CASES = ('launch','sequential','one-seek','two-seeks','chunk-below','chunk-at','chunk-above','working-below','working-at','working-above','entries-below','entries-at','entries-above','shared-below','shared-at','shared-above','phase-below','phase-at','phase-above','phase-history-below','phase-history-at','phase-history-above','entries-above-repeated','phase-demand-below','phase-demand-at','phase-demand-above')
CAPACITIES = (0, 2*1024*1024)
CASES += ('parked-below', 'parked-at', 'parked-above', 'parked-working')
FIXTURE = cli.ROOT/'benchmarks/fixtures/native-launch-ranges.json'


def parse_sample(stdout, case, capacity, cycles):
    rows = [json.loads(line.removeprefix('seek_replay_sample ')) for line in stdout.splitlines()
            if line.startswith('seek_replay_sample ')]
    if len(rows)!=1 or '1 passed;' not in stdout:
        raise common.BenchmarkError('replay probe did not execute exactly one passing case')
    row=rows[0]
    if (row.get('case'),row.get('capacity'),row.get('cycles'))!=(case,capacity,cycles):
        raise common.BenchmarkError('replay configuration differs')
    if row.get('correctness')!='passed' or row.get('release')!='passed':
        raise common.BenchmarkError('replay correctness/release gate failed')
    for key in ('elapsed_ns','decode_calls','decoded_bytes','fetch_ns','admission_ns','decode_ns','chunk_range_requests','cache_bytes','cache_entries','returned_bytes'):
        if not isinstance(row.get(key),int) or row[key]<0:
            raise common.BenchmarkError('invalid replay metric: '+key)
    readers={'shared-below':15,'shared-at':16,'shared-above':17}.get(case,1)
    if row.get('reader_count') != readers or not isinstance(row.get('shared_cache_bytes'),int):
        raise common.BenchmarkError('invalid reader count or shared cache measurement')
    if not row['cache_bytes'] <= row['shared_cache_bytes'] <= 32*1024*1024:
        raise common.BenchmarkError('exceeded shared cache bound')
    if case.startswith('shared-') and row['cache_bytes'] != min(capacity*readers,32*1024*1024):
        raise common.BenchmarkError('shared cache boundary was not exercised')
    if row['elapsed_ns']==0 or row['cache_bytes']>capacity*readers or row['cache_entries']>64*readers:
        raise common.BenchmarkError('invalid timing or exceeded cache bound')
    if case in ('parked-below', 'parked-at') and capacity == 2*1024*1024 and row['decode_calls'] != 2:
        raise common.BenchmarkError('parked sequential reads repeatedly decoded the same chunk')
    if case.startswith('phase-'):
        warm={'phase-below':2097151,'phase-at':2097152,'phase-above':2097153}.get(case,2097152)
        if row.get('phase_warm_bytes') != warm or row.get('warm_cache_bytes') != (warm if warm <= capacity else 0):
            raise common.BenchmarkError('phase transition did not exercise the admission boundary')
        if case.startswith('phase-history-') and row.get('phase_hot_chunks') != {'phase-history-below':63,'phase-history-at':64,'phase-history-above':65}[case]:
            raise common.BenchmarkError('invalid phase history boundary')
        for key in ('warm_elapsed_ns','measured_decode_calls'):
            if not isinstance(row.get(key),int) or row[key] <= 0:
                raise common.BenchmarkError('invalid phase measurement: '+key)
    return row


def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile',choices=('smoke','standard'),default='standard')
    parser.add_argument('--repetitions',type=int,default=3)
    parser.add_argument('--input',type=Path,help='optional exact measured awk executable; otherwise deterministic synthetic bytes')
    parser.add_argument('--probe-binary',type=Path)
    parser.add_argument('--no-build',action='store_true')
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--report',type=Path)
    args=parser.parse_args(argv)
    if args.repetitions<1: parser.error('positive repetitions required')
    if args.no_build and args.probe_binary is None: parser.error('--no-build requires --probe-binary')
    fixture=json.loads(FIXTURE.read_text())
    input_receipt=None
    if args.input:
        args.input=args.input.resolve()
        data=args.input.read_bytes()
        input_receipt={'path':str(args.input),'sha256':hashlib.sha256(data).hexdigest(),'size':len(data)}
        if input_receipt['sha256']!=fixture['tool_sha256'] or len(data)!=fixture['tool_size']:
            parser.error('--input must match the measured executable hash and size in the range fixture')
    binary=args.probe_binary
    if binary is None:
        built=subprocess.run(['cargo',*CARGO_ARGUMENTS],cwd=cli.ROOT,capture_output=True,text=True)
        if built.returncode: raise common.BenchmarkError(built.stderr or built.stdout)
        binary=parse_probe_binary(built.stdout)
    binary=Path(binary).resolve()
    result={'schema_version':1,'result_schema':'casita.decoded-seek-replay.v1','complete':False,
            'configuration':{'profile':args.profile,'repetitions':args.repetitions,'cases':CASES,'capacities':CAPACITIES,
                             'order':fixture['order'],'compressed_cache_bytes':8*1024*1024,'compressed_cache':'warmed before timing',
                             'decode_budget_bytes':64*1024*1024,'shared_decoded_cache_bytes':32*1024*1024},
            'input':input_receipt,'fixture':fixture,'artifacts':[{'path':str(binary),'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}],
            'source_sha256':{str(p.relative_to(cli.ROOT)):hashlib.sha256(p.read_bytes()).hexdigest()
                             for p in [*sorted((cli.ROOT/'crates/casita/src').rglob('*.rs')), cli.ROOT/'Cargo.toml', cli.ROOT/'crates/casita/Cargo.toml', cli.ROOT/'Cargo.lock', FIXTURE]},
            'samples':[]}
    def save(): common.write_atomic(args.output,json.dumps(result,indent=2)+'\n')
    with tempfile.TemporaryDirectory(prefix='casita-seek-replay-') as temporary:
        result['environment']={'captured_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
                               'hostname':platform.node(),'platform':platform.platform(),
                               'architecture':platform.machine(),'python':platform.python_version(),
                               'uid':os.getuid(),'logical_cpus':os.cpu_count(),'load':os.getloadavg(),
                               'rustc':common.command_version(['rustc','--version']),
                               'cargo':common.command_version(['cargo','--version'])}
        save()
        schedule=[(r,c,n) for r in range(args.repetitions) for c in CASES for n in CAPACITIES]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition,case,capacity in schedule:
                cycles={'phase-demand-below':7,'phase-demand-at':8,'phase-demand-above':9}.get(case) or (1 if case in ('sequential','one-seek','two-seeks') or case.startswith('parked-') else 3 if args.profile=='smoke' else 17)
                env={k:v for k,v in os.environ.items() if k!='CASITA_SEEK_INPUT'}
                env.update(CASITA_SEEK_CASE=case,CASITA_SEEK_CAPACITY=str(capacity),CASITA_SEEK_CYCLES=str(cycles))
                if args.input: env['CASITA_SEEK_INPUT']=str(args.input)
                command=[str(binary),PROBE,'--exact','--ignored','--nocapture']
                process=subprocess.run(command,cwd=cli.ROOT,env=env,capture_output=True,text=True,timeout=180)
                if process.returncode: raise common.BenchmarkError(process.stdout+process.stderr)
                row=parse_sample(process.stdout,case,capacity,cycles)
                result['samples'].append({**row,'repetition':repetition,'status':'ok','operation':case,
                                          'wall_seconds':row['elapsed_ns']/1e9,'command':command})
                save()
            result['complete']=True
        except BaseException as error:
            result['error']=str(error)
            raise
        finally: save()
    if args.report:
        common.write_atomic(args.report,'# Decoded seek replay\n\nComplete: '+str(result['complete'])+'\n\nSee '+str(args.output)+' for phase measurements and correctness receipts.\n')
    print(f'decoded-seek-replay: {len(result["samples"])} checked cases; {args.output}')
    return 0


if __name__=='__main__':
    raise SystemExit(main())
