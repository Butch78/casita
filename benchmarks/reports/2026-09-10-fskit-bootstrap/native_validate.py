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
    (root / 'native-validation.json').write_text(json.dumps(report, indent=2) + '\n')

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

assert not mounted(), 'FSKit volumes must be unmounted before validation'
assert not version.exists(), 'validation requires no existing Casita installation'
(root / 'enabledModules.before.plist').write_bytes(settings.read_bytes())
original_modules = modules()
save()
try:
    # Stop the installer after checksum verification, while extraction is staged.
    job = start('interrupted-install', [setup])
    deadline = time.monotonic() + 120
    staged = []
    while time.monotonic() < deadline and job[1].poll() is None:
        staged = list(base.glob('.install-*'))
        if staged:
            os.killpg(job[1].pid, signal.SIGTERM)
            break
        time.sleep(0.002)
    code = finish(job)
    assert staged and code != 0 and not version.exists(), 'missed staged interruption window'
    assert not (base / 'setup-v1.json').exists(), 'interrupted install published a receipt'
    package = base / 'fuse-t-1.2.7.pkg'
    report['interrupted_installer_sha256'] = hashlib.sha256(package.read_bytes()).hexdigest()
    report['abandoned_staging'] = list(map(str, staged))
    assert modules() == original_modules

    # Exercise real activation rather than silently relying on the old setup.
    current = modules()
    if module in current:
        subprocess.run(['/usr/libexec/PlistBuddy', '-c', f'Delete :{current.index(module)}', str(settings)], check=True)
    assert module not in modules()
    benchmark = [fs_test, '--exact', 'darwin::setup::tests::benchmark_reuse', '--ignored', '--nocapture']
    mounts = [builder_test, '--exact', 'local::tests::darwin_fskit_simultaneous_mounts', '--ignored', '--nocapture']
    jobs = [start('concurrent-bootstrap', benchmark), start('simultaneous-mounts', mounts)]
    codes = [finish(job) for job in jobs]
    assert codes == [0, 0], codes
    assert (base / 'setup-v1.json').is_file()
    assert module in modules()
    assert set(original_modules).issubset(modules())
    assert not mounted()
    report['receipt'] = json.loads((base / 'setup-v1.json').read_text())

    for index in range(3):
        assert finish(start(f'new-process-reuse-{index}', benchmark)) == 0
    assert finish(start('explicit-reuse', [setup])) == 0
    assert finish(start('explicit-repair', [setup, '--repair'])) == 0
    assert finish(start('post-repair-reuse', benchmark)) == 0
    assert finish(start('obrador-build-reuse-isolation', [builder_test, '--exact',
        'local::tests::darwin_fskit_build_reuse_and_isolation', '--ignored', '--nocapture']), 240) == 0
    assert not mounted()
    report['status'] = 'passed'
except BaseException as error:
    report['status'] = 'failed'
    report['error'] = repr(error)
    raise
finally:
    # Preserve all unrelated entries, and restore original module enablement if
    # setup stopped before it could do so. Do not overwrite concurrent settings.
    if module in original_modules and module not in modules():
        subprocess.run(['/usr/libexec/PlistBuddy', '-c', f'Add : string {module}', str(settings)], check=True)
    report['remaining_fskit_mounts'] = mounted()
    report['final_modules'] = modules()
    save()
