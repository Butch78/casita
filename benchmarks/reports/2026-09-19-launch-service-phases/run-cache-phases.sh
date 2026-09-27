set -euo pipefail
source /Users/hetzner/casita-native-fskit.w9GGjK/environment.sh
cd /Users/hetzner/casita-native-repository.hqI8w3
probe=$(python3 -c 'from benchmarks.suites.pack.catalog import parse_probe_binary; print(parse_probe_binary(open("results/cache-phase-build.jsonl").read()))')
"$probe" blob::pack::fetch:: --nocapture > results/cache-phase-tests.log 2>&1
python3 -m benchmarks.suites.decoded_seek_replay --profile standard --repetitions 3 --input /nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk --probe-binary "$probe" --no-build --output results/cache-phase-awk.json
python3 -m benchmarks.suites.decoded_seek_replay --profile smoke --repetitions 1 --probe-binary "$probe" --no-build --output results/cache-phase-synthetic.json
