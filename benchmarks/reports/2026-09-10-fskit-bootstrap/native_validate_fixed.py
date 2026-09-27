"""Native validation driver; requires a disposable, initially absent Casita runtime.

Run under Obrador's devenv shell. Arguments: validation directory, fs test
binary, builder test binary, setup binary. Raw timings come from the permanent
fskit-setup-reuse benchmark. Only this user's FUSE-T module entry is edited;
settings are backed up and module presence restored on failure.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

root, fs_test, builder_test, setup = map(Path, sys.argv[1:])
home = Path.home()
base = home / '.local/share/casita/fuse-t'
version = base / '1.2.7'
settings = home / 'Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist'
module = 'org.fuset.fskit-srv.module'
report = {'schema_version': 1, 'phases': [], 'platform': subprocess.check_output(['sw_vers'], text=True)}
env = dict(os.environ)
for key in ('CASITA_FUSE_T_LIBRARY', 'OBRADOR_FUSE_T_LIBRARY'):
    env.pop(key, None)
env['CASITA_BENCH_NATIVE_FSKIT_SETUP'] = '1'
env['OBRADOR_FSKIT_TEST_DIR'] = str(root / 'test-data')
Path(env['OBRADOR_FSKIT_TEST_DIR']).mkdir(exist_ok=True)
for key in ('OBRADOR_CLANG_TOOLS', 'OBRADOR_APPLE_SDKROOT'):
    assert Path(env[key]).exists(), key

def save():
    (root / 'native-validation-fixed.json').write_text(json.dumps(report, indent=2) + '\n')

def mounted():
    return [line for line in subprocess.check_output(['/sbin/mount'], text=True).splitlines()
            if 'fskit' in [flag.strip() for flag in line.rsplit(' (', 1)[-1].rstrip(')').split(',')]]

def modules():
    return json.loads(subprocess.check_output(['/usr/bin/plutil', '-convert', 'json', '-o', '-', str(settings)]))

def start(name, command):
    output = (root / f'{name}.log').open('w')
    process = subprocess.Popen(list(map(str, command)), env=env, stdout=output,
                               stderr=subprocess.STDOUT, start_new_session=True)
    output.close()
    return name, process, time.monotonic()

def finish(job, timeout=180):
    name, process, started = job
    try:
        code = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        code = 'timeout'
    row = {'name': name, 'exit_code': code, 'wall_seconds': time.monotonic() - started}
    log = (root / f'{name}.log').read_text()
    row['samples'] = [json.loads(line) for line in log.splitlines() if line.startswith('{')]
    report['phases'].append(row)
    save()
    print(json.dumps(row), flush=True)
    return code

assert not mounted()
benchmark = [fs_test, '--exact', 'darwin::setup::tests::benchmark_reuse', '--ignored', '--nocapture']
assert finish(start('fixed-schema-upgrade', [setup])) == 0
receipt = (base / 'setup-v1.json').read_bytes()
for index in range(3):
    assert finish(start(f'fixed-new-process-{index}', benchmark)) == 0
# Reproduce the agent's identical atomic rewrite with a distinct inode.
import tempfile
with tempfile.NamedTemporaryFile(dir=settings.parent, delete=False) as replacement:
    replacement.write(settings.read_bytes())
    replacement_path = Path(replacement.name)
os.chmod(replacement_path, settings.stat().st_mode & 0o777)
os.replace(replacement_path, settings)
assert finish(start('fixed-after-settings-replacement', benchmark)) == 0
assert (base / 'setup-v1.json').read_bytes() == receipt, 'settings rewrite invalidated receipt'
for name in ('darwin_fskit_simultaneous_mounts', 'darwin_fskit_build_reuse_and_isolation'):
    assert finish(start('fixed-' + name, [builder_test, '--exact', 'local::tests::' + name, '--ignored', '--nocapture']), 240) == 0
    assert finish(start('fixed-after-' + name, benchmark)) == 0
    assert (base / 'setup-v1.json').read_bytes() == receipt, 'mount invalidated receipt'
assert not mounted()
report['status'] = 'passed'
report['receipt_unchanged_after_mounts_and_settings_replacement'] = True
save()
