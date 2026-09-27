import json
import os
from benchmarks.suites.native_fskit_workloads import main

previous = json.load(open('results/workloads-adaptive-32.json'))
os.environ.update({'CASITA_WORKLOAD_'+k.upper(): v['source']
                  for k, v in previous['workload_fixture']['tools'].items()})
raise SystemExit(main(['--profile', 'standard', '--repetitions', '3',
                      '--metadata-files', '0', '--timeout-seconds', '3600',
                      '--trace-read-ranges', '--workload-workers', '1', '17', '32', '33',
                      '--output', 'results/workloads-callbacks.json']))
