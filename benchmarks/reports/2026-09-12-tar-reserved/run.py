"""Run the existing tar matrix with user-authorized build suspension on Linux.

Usage: python3 run.py BINARY NEW_OUTPUT_DIRECTORY
Only invoke after permission to pause other local builds. SIGCONT is sent on
exit, with a separate watchdog as a fallback if the controller is killed.
"""
import json
import os
import pathlib
import signal
import subprocess
import sys
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from benchmarks.host_activity import processes


def resume(entries):
    current = processes()
    result = []
    for entry in reversed(entries):
        pid = entry['pid']
        status = 'exited'
        if pid in current and current[pid]['started'] == entry['started']:
            try:
                os.kill(pid, signal.SIGCONT)
                status = 'resumed'
            except ProcessLookupError:
                pass
        result.append({**entry, 'status': status})
    return result


def watchdog(ledger):
    while True:
        state = json.loads(ledger.read_text())
        if state.get('finished'):
            return
        owner = processes().get(state['controller_pid'])
        if (owner is None or owner['started'] != state['controller_started']
                or time.time() >= state['resume_deadline']):
            result = resume(state['paused'])
            ledger.with_suffix('.watchdog.json').write_text(json.dumps(result, indent=2) + '\n')
            if owner and owner['started'] == state['controller_started']:
                os.kill(state['controller_pid'], signal.SIGTERM)
            return
        time.sleep(2)


def main(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    ledger = output / 'paused-builds.json'
    state = {'controller_pid': os.getpid(), 'controller_started': processes()[os.getpid()]['started'],
             'resume_deadline': time.time() + 900, 'paused': [], 'finished': False}
    def save():
        pending = ledger.with_suffix('.tmp')
        pending.write_text(json.dumps(state, indent=2) + '\n')
        pending.replace(ledger)
    save()
    guard = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).resolve()), '--watchdog', str(ledger)],
                             start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    stop = threading.Event()
    errors = []
    def pause_builds():
        try:
            while not stop.is_set():
                current = processes()
                own = {os.getpid()}
                while True:
                    children = {pid for pid, p in current.items() if p['parent'] in own}
                    if children <= own:
                        break
                    own |= children
                names = {'cargo', '.cargo-wrapped', 'cargo-clippy', '.cargo-clippy-w',
                         'rustc', 'clippy-driver', 'rust-lld', 'cc1', 'cc1plus', 'obrador-closure'}
                selected = set()
                for pid, p in current.items():
                    if pid not in own and p['name'] in names:
                        try:
                            if pathlib.Path(f'/proc/{pid}').stat().st_uid == os.getuid():
                                selected.add(pid)
                        except FileNotFoundError:
                            pass
                while True:
                    children = {pid for pid, p in current.items() if p['parent'] in selected}
                    if children <= selected:
                        break
                    selected |= children
                # Parents precede children; repeated scans catch any late forks.
                def depth(pid):
                    count, seen = 0, set()
                    while pid in selected and pid not in seen:
                        seen.add(pid)
                        pid = current[pid]['parent']
                        count += 1
                    return count
                for pid in sorted(selected, key=depth):
                    p = current[pid]
                    if p.get('state') in ('T', 't'):
                        continue
                    latest = processes().get(pid)
                    if latest is None or latest['started'] != p['started']:
                        continue
                    if not any(e['pid'] == pid and e['started'] == p['started'] for e in state['paused']):
                        entry = dict(pid=pid, name=p['name'], parent=p['parent'], started=p['started'])
                        try:
                            entry['cwd'] = os.readlink(f'/proc/{pid}/cwd')
                        except OSError:
                            pass
                        state['paused'].append(entry)
                        save()  # The watchdog can resume even if the controller dies after SIGSTOP.
                    try:
                        os.kill(pid, signal.SIGSTOP)
                    except ProcessLookupError:
                        pass
                stop.wait(1)
        except Exception as error:
            errors.append(str(error))
            os.kill(os.getpid(), signal.SIGTERM)
    def terminate(*_):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, terminate)
    thread = threading.Thread(target=pause_builds, daemon=True)
    child = None
    try:
        command = [sys.executable, '-m', 'benchmarks.tar_compare', '--binary', str(binary),
                   '--output', str(output / 'timing'), '--repetitions', '4', '--quiet-timeout', '180']
        state['command'] = command
        thread.start()
        # The pause thread owns ledger writes until it is joined below.
        with (output / 'runner.log').open('w') as log:
            child = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
            code = child.wait(timeout=840)
        return code
    finally:
        try:
            if child is not None and child.poll() is None:
                child.send_signal(signal.SIGINT)
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.terminate()
                    child.wait(timeout=10)
        finally:
            stop.set()
            thread.join()
            state['resume_results'] = resume(state['paused'])
            state.update(finished=True, errors=errors)
            save()
            guard.wait(timeout=5)
            print('Resumed paused builds; ledger:', ledger, flush=True)


if __name__ == '__main__':
    if sys.argv[1] == '--watchdog':
        watchdog(pathlib.Path(sys.argv[2]))
    else:
        raise SystemExit(main(pathlib.Path(sys.argv[1]).resolve(), pathlib.Path(sys.argv[2]).resolve()))
