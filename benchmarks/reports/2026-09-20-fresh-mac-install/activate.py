"""Development activation test only; does not validate signed distribution."""
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys

work = Path(sys.argv[1]).resolve()
report = {"uid": os.getuid(), "commands": [], "complete": False}
assert os.getuid() != 0

def run(args):
    p = subprocess.run(list(map(str, args)), capture_output=True, text=True, timeout=60)
    report['commands'].append(dict(argv=list(map(str, args)), returncode=p.returncode,
                                   stdout=p.stdout, stderr=p.stderr))
    return p

try:
    mounts = run(['/sbin/mount']).stdout
    assert not any('fskit' in line.rsplit(' (', 1)[-1].rstrip(')').split(', ')
                   for line in mounts.splitlines()), 'Existing FSKit mount; stop activation'
    settings = Path.home() / 'Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist'
    original = settings.read_bytes() if settings.exists() else None
    modules = plistlib.loads(original) if original else []
    assert isinstance(modules, list) and all(isinstance(m, str) for m in modules)
    identifier = 'org.casita.native-fskit.extension.repository'
    report['settings_original'] = modules[:]
    if identifier not in modules:
        if original is not None:
            (work / 'enabledModules.original.plist').write_bytes(original)
        modules.append(identifier)
        settings.parent.mkdir(parents=True, exist_ok=True)
        assert (settings.read_bytes() if settings.exists() else None) == original
        settings.write_bytes(plistlib.dumps(modules))
    run(['/sbin/mount', '-F', '-t', 'casitarepo', work / 'repository', work / 'mount'])
    report['mounted'] = os.path.ismount(work / 'mount')
    if report['mounted']:
        tree = work / 'mount/views/fixture'
        report['entries'] = sorted(p.name for p in tree.iterdir())
        report['size_4096'] = len((tree / 'size-4096').read_bytes())
        assert report['size_4096'] == 4096
        report['stats'] = json.loads((work / 'mount/__casita_stats-0').read_text())
        assert run(['/sbin/umount', work / 'mount']).returncode == 0
        assert not os.path.ismount(work / 'mount')
        report['complete'] = True
finally:
    (work / 'activation-report.json').write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(report, indent=2))
