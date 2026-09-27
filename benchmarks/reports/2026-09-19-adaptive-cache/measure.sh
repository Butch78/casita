set -euo pipefail
source /Users/hetzner/casita-native-fskit.w9GGjK/environment.sh
cd /Users/hetzner/casita-native-repository.hqI8w3
python3 results/pin-fixture-tools.py
export CASITA_NATIVE_TARGET_DIR=/Users/hetzner/casita-native-repository.hqI8w3/target
probe=$(python3 -c 'from benchmarks.suites.pack.catalog import parse_probe_binary; print(parse_probe_binary(open("results/adaptive-build.jsonl").read()))')
for filter in blob::chunked_reader::tests:: blob::pack::fetch:: blob::chunked::tests:: object_read_tests::; do
    "$probe" "$filter" --nocapture
done > results/adaptive-tests.log 2>&1
python3 -m benchmarks.suites.decoded_seek_replay --profile standard --repetitions 3 --input /nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk --probe-binary "$probe" --no-build --output results/adaptive-awk.json
python3 -m benchmarks.suites.decoded_seek_replay --profile smoke --repetitions 1 --probe-binary "$probe" --no-build --output results/adaptive-synthetic.json
cargo test --manifest-path casita-fs/evaluations/native-fskit/Cargo.toml --release --features repository,fuser-baseline --lib repository::tests:: --target-dir target > results/adaptive-native-tests.log 2>&1
python3 - <<'PY'
import json,os
from benchmarks.suites.native_fskit_workloads import main
r=json.load(open('results/workloads-stable-cache.json'))
os.environ.update({'CASITA_WORKLOAD_'+k.upper():v['source'] for k,v in r['workload_fixture']['tools'].items()})
reuse=[]
for capacity in (16,32):
    output=f'results/workloads-adaptive-{capacity}.json'
    status=main(['--profile','standard','--repetitions','3','--metadata-files','0','--timeout-seconds','3600','--trace-read-ranges','--reader-cache-capacity',str(capacity),'--output',output,*(['--workload-workers','17'] if capacity==16 else []),*reuse])
    if status: raise SystemExit(status)
    r=json.load(open(output))
    reuse=['--bundle',r['bundle'],'--server-binary',r['server_binary']]
PY
