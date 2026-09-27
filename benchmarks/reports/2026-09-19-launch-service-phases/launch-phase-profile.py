import json,os
from benchmarks.suites.native_fskit_workloads import main
r=json.load(open('results/workloads-stable-cache.json'))
os.environ.update({'CASITA_WORKLOAD_'+k.upper():v['source'] for k,v in r['workload_fixture']['tools'].items()})
raise SystemExit(main(['--profile','standard','--repetitions','3','--metadata-files','0','--timeout-seconds','2400','--trace-read-ranges','--bundle',r['bundle'],'--server-binary',r['server_binary'],'--output','results/workloads-stable-cache-phases.json']))
