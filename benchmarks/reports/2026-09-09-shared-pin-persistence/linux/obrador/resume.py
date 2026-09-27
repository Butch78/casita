import json,pathlib,subprocess,time
root=pathlib.Path(__file__).resolve().parent
execution=json.loads((root/'execution.json').read_text())
template=execution['runs'][0]['command']
for label,variant,traced in [('round-3-retry-baseline','baseline',False),('round-3-retry-after','after',False),('trace-baseline','baseline',True),('trace-after','after',True)]:
 command=template.copy()
 command[command.index('--plugin')+1]=str(root/f'{variant}.so')
 command[command.index('--output')+1]=str(root/label)
 if traced:
  command.remove('nix')
  command += ['--trace','obrador=debug,obrador_core=debug,casita=debug']
 run={'label':label,'variant':variant,'traced':traced,'command':command,'started':time.time(),'uptime':subprocess.check_output(['uptime'],text=True),'note':'Fresh matched pair after original round-3-after preparation timed out; original evidence retained.'}
 execution['runs'].append(run);(root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
 print(label,flush=True)
 with (root/f'{label}.log').open('w') as log:r=subprocess.run(command,cwd='/tmp/obrador-shared-pins-source',stdout=log,stderr=subprocess.STDOUT)
 run.update(exit_code=r.returncode,finished=time.time());(root/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
 if r.returncode:raise SystemExit(r.returncode)
