import json,os
from benchmarks.suites.native_fskit_workloads import main
r=json.load(open('results/workloads-adaptive-16.json'))
os.environ.update({'CASITA_WORKLOAD_'+k.upper():v['source'] for k,v in r['workload_fixture']['tools'].items()})
raise SystemExit(main(['--profile','standard','--repetitions','3','--metadata-files','0','--timeout-seconds','3600','--trace-read-ranges','--reader-cache-capacity','32','--bundle',r['bundle'],'--server-binary',r['server_binary'],'--output','results/workloads-adaptive-32.json']))
