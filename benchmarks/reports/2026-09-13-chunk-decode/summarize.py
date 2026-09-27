"""Retain correctness, provenance, capacity, and timing admission evidence.

Usage: python3 summarize.py RAW_RESULT_DIRECTORY OUTPUT_JSON
"""
import hashlib
import json
import pathlib
import sys


def main(raw, output):
    logs = {name: (raw / name).read_text() for name in (
        'chunk-tests.log', 'tar-tests.log', 'harness-tests.log', 'clippy.log',
        'fmt.log', 'decoder-correctness.log', 'tar-correctness.log', 'build.log')}
    preflight = [json.loads(line) for line in logs['decoder-correctness.log'].splitlines()
                 if line.startswith('{') and
                 json.loads(line).get('schema') == 'casita.chunk-decompression.v1']
    assert len(preflight) == 108
    assert all(row['correctness'] == 'passed' for row in preflight)
    assert logs['decoder-correctness.log'].count('Success') == 108
    assert logs['tar-correctness.log'].count('Success') == 16
    assert '75 passed; 0 failed' in logs['chunk-tests.log']
    assert '39 passed; 0 failed' in logs['tar-tests.log']
    assert 'Ran 232 tests' in logs['harness-tests.log']
    assert 'OK (skipped=2)' in logs['harness-tests.log']
    assert 'Finished `dev` profile' in logs['clippy.log']
    report = {
        'schema': 'casita.chunk-decode-investigation.v1',
        'provenance': json.loads((raw / 'provenance.json').read_text()),
        'preflight': preflight,
        'decoder_comparison': json.loads((raw / 'decoder-paired/report.json').read_text()),
        'tar_comparison': json.loads((raw / 'tar-paired/report.json').read_text()),
        'logs': logs,
        'interpretation': {
            'capacity': 'Output Vec capacity only; excludes decoder contexts and other allocations.',
            'timing': 'Only complete accepted comparisons establish timing results; admission failures do not.',
        },
        'reproduction_source_sha256': {
            name: hashlib.sha256(pathlib.Path(__file__).with_name(name).read_bytes()).hexdigest()
            for name in ('run.py', 'summarize.py')},
    }
    output.write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]))
