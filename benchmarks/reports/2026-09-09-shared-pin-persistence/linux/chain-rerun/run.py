import hashlib, json, pathlib, subprocess, time
root = pathlib.Path(__file__).resolve().parent
plugins = pathlib.Path('/tmp/obrador-shared-pins-linux')
runner = pathlib.Path('/tmp/obrador-shared-pins-source/benchmarks/builds/compare.py')
env = '/tmp/obrador-shared-pins-env.sh'
nix = '/nix/store/n4sps2fl79lqrms8mjvqg0wd7i07pk55-nix-2.36.0pre20260901_f8cd4ce/bin/nix'
shell = '/nix/store/a48z5j76wm1n36yq87hsxh3byf2zq4h1-busybox-1.37.0/bin/busybox'
execution = {'workloads':['chain'], 'count':64, 'jobs':8, 'baseline':'59c2fbdcb1f10b0df607e5976c797f39e776ce3f','after':'4e7b5be711e3f6b8780d5aedd60b6af99498e141','runs':[],'plugins':{v:hashlib.sha256((plugins/f'{v}.so').read_bytes()).hexdigest() for v in ['baseline','after']}}
schedule = [(f'round-{r}-{v}',v,False) for r in range(1,4) for v in (['baseline','after'] if r % 2 else ['after','baseline'])]

for label, variant, traced in schedule:
    command = ['bash',env,'python3',str(runner),'--nix',nix,'--shell',shell,'--plugin',str(plugins/f'{variant}.so'),'--workloads','chain','--count','64','--jobs','8','--rounds','1','--backends',*(['obrador'] if traced else ['nix','obrador']),'--timeout','900','--output',str(root/label)]
    if traced: command += ['--trace','obrador=debug,obrador_core=debug,casita=debug']
    run={'label':label,'variant':variant,'traced':traced,'command':command,'started':time.time(),'uptime':subprocess.check_output(['uptime'],text=True)}
    execution['runs'].append(run)
    (root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
    print(label,flush=True)
    with (root/f'{label}.log').open('w') as log: result=subprocess.run(command,cwd=runner.parents[2],stdout=log,stderr=subprocess.STDOUT)
    run.update(exit_code=result.returncode,finished=time.time())
    (root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
    if result.returncode: raise SystemExit(result.returncode)
