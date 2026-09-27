"""Compare preserved filesystem_import executables without compiling during timing."""
import argparse, hashlib, json, os, platform, subprocess, time
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--baseline',type=Path,required=True)
p.add_argument('--candidate',type=Path,required=True)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args(); a.output.mkdir(parents=True,exist_ok=False)
result={'platform':platform.platform(),'uid':os.getuid(),'binaries':{},'runs':[]}
for name in ['baseline','candidate']:
 binary=getattr(a,name).resolve()
 result['binaries'][name]={'path':str(binary),'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
for rep in range(2):
 for name in (['baseline','candidate'] if rep==0 else ['candidate','baseline']):
  out=a.output/f'{name}-{rep}';out.mkdir()
  command=[result['binaries'][name]['path'],'--bench','--warm-up-time','0.2','--measurement-time','0.5','--sample-size','10','--noplot']
  started=time.monotonic(); load=os.getloadavg()
  print(f'Running {name}, round {rep}',flush=True)
  with (out/'run.log').open('w') as f:
   run=subprocess.Popen(command,env={**os.environ,'CRITERION_HOME':str(out.resolve()/'criterion')},stdout=f,stderr=subprocess.STDOUT)
   _,status,usage=os.wait4(run.pid,0)
   run.returncode=os.waitstatus_to_exitcode(status)
  samples=[]
  for est in sorted((out/'criterion').glob('**/new/estimates.json')):
   value=json.loads(est.read_text()); meta=json.loads(est.with_name('benchmark.json').read_text())
   samples.append({'case':meta['full_id'],'median_ns':value['median']['point_estimate'],'median_interval':value['median']['confidence_interval']})
  result['runs'].append({'variant':name,'round':rep,'command':command,'wall_seconds':time.monotonic()-started,'exit_code':run.returncode,'process_cpu_seconds':usage.ru_utime+usage.ru_stime,'peak_rss_bytes':usage.ru_maxrss*(1 if platform.system()=='Darwin' else 1024),'load_before':load,'load_after':os.getloadavg(),'samples':samples})
  (a.output/'results.json').write_text(json.dumps(result,indent=2)+'\n')
  assert run.returncode==0 and len(samples)==12,(run.returncode,len(samples))
