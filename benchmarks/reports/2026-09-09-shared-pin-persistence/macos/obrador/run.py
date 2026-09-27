import hashlib, json, pathlib, subprocess, time
root = pathlib.Path(__file__).resolve().parent
python = '/nix/store/mfkdmplffnbc0av8r6pknl796a6b1r2n-python3-3.13.13/bin/python3'
nix = '/nix/store/p805hs6b93ij2xz2axvb7b1xyds58yln-nix-2.36.0pre20260901_f8cd4ce/bin/nix'
runner = root/'source/benchmarks/builds/compare.py'
execution = {'baseline': '59c2fbdcb1f10b0df607e5976c797f39e776ce3f', 'after': '4e7b5be711e3f6b8780d5aedd60b6af99498e141', 'runs': [], 'plugins': {v: hashlib.sha256((root/f'{v}.dylib').read_bytes()).hexdigest() for v in ['baseline','after']}}
schedule = [(f'round-{r}-{v}',v,False) for r in range(1,4) for v in (['baseline','after'] if r % 2 else ['after','baseline'])]
schedule += [(f'trace-{v}',v,True) for v in ['baseline','after']]
for label, variant, traced in schedule:
    command = ['bash',str(root/'env.sh'),python,str(runner),'--nix',nix,'--plugin',str(root/f'{variant}.dylib'),'--workloads','chain','wide','--count','64','--jobs','8','--rounds','1','--backends',*(['obrador'] if traced else ['nix','obrador']),'--timeout','900','--output',str(root/label)]
    if traced: command += ['--trace','obrador=debug,obrador_core=debug,casita=debug']
    run = {'label':label,'variant':variant,'traced':traced,'command':command,'started':time.time(),'uptime':subprocess.check_output(['uptime'],text=True)}
    execution['runs'].append(run)
    (root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
    print(label,flush=True)
    with (root/f'{label}.log').open('w') as log:
        result = subprocess.run(command,cwd=root/'source',stdout=log,stderr=subprocess.STDOUT)
    run.update(exit_code=result.returncode,finished=time.time())
    (root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
    if result.returncode: raise SystemExit(result.returncode)
